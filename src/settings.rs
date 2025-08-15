use core::{ops::Range, str::Utf8Error};
use embedded_storage_async::nor_flash::NorFlash;
use minicbor::encode::write::EndOfSlice;
use sequential_storage::{cache::NoCache, map::Value};

type SettingKey = u32;

const DATA_FORMAT_STRING: &str = "settings-0.0";

#[derive(Debug)]
pub struct UninitializedSettings<'s, const DATA_BUF_LEN: usize, S>
where
    S: NorFlash,
{
    inner: Settings<'s, DATA_BUF_LEN, S>,
}
impl<'s, const DATA_BUF_LEN: usize, S> UninitializedSettings<'s, DATA_BUF_LEN, S>
where
    S: NorFlash,
{
    /// Initialize the underlying storage layout for use
    pub async fn init(
        mut self,
    ) -> Result<Settings<'s, DATA_BUF_LEN, S>, SettingsError<sequential_storage::Error<S::Error>>>
    {
        self.inner.verify_load().await?;
        Ok(self.inner)
    }

    /// Blocking version of [`init()`]
    pub fn init_blocking(
        mut self,
    ) -> Result<Settings<'s, DATA_BUF_LEN, S>, SettingsError<sequential_storage::Error<S::Error>>>
    {
        let fut = self.inner.verify_load();
        embassy_futures::block_on(fut)?;
        Ok(self.inner)
    }

    /// Clears the partition and initializes the storage layout for use
    pub async fn reset(
        mut self,
    ) -> Result<Settings<'s, DATA_BUF_LEN, S>, SettingsError<sequential_storage::Error<S::Error>>>
    {
        self.inner
            .storage
            .erase(self.inner.storage_range.start, self.inner.storage_range.end)
            .await
            .map_err(|e| sequential_storage::Error::Storage { value: e })?;

        sequential_storage::map::store_item::<SettingKey, &[u8], _>(
            &mut self.inner.storage,
            self.inner.storage_range.clone(),
            &mut self.inner.cache,
            self.inner.data_buffer,
            &0u32,
            &DATA_FORMAT_STRING.as_bytes(),
        )
        .await?;
        Ok(self.inner)
    }
}

#[derive(Debug)]
pub struct Settings<'s, const DATA_BUF_LEN: usize, S>
where
    S: NorFlash,
{
    storage: S,
    storage_range: Range<u32>,
    data_buffer: &'s mut [u8; DATA_BUF_LEN],
    cache: NoCache,
}

#[derive(Debug, defmt::Format)]
pub enum SettingsError<E> {
    /// Attempted to use settings before initialization
    NotReady,
    /// No persisted configuration was found
    NotFound,
    /// The found data was corrupt or invalid
    CorruptOrInvalid,
    /// The setting data could not be converted to a string
    StrConversionError(#[defmt(Debug2Format)] Utf8Error),
    DecodeError(#[defmt(Debug2Format)] minicbor::decode::Error),
    DeserializeError(#[defmt(Debug2Format)] minicbor_serde::error::DecodeError),
    InnerError(#[defmt(Debug2Format)] E),
    SerializeError(
        #[defmt(Debug2Format)]
        minicbor_serde::error::EncodeError<minicbor::encode::write::EndOfSlice>,
    ),
}
impl<E> From<E> for SettingsError<E> {
    fn from(value: E) -> Self {
        Self::InnerError(value)
    }
}

impl<'s, const DATA_BUF_LEN: usize, S> Settings<'s, DATA_BUF_LEN, S>
where
    S: NorFlash,
{
    /// Create a new settings object
    ///
    /// This does not yet mutate the storage.
    /// Call [`init()`] to initialize the storage for use. Only then [`get()`] and [`set()`] can be used.
    pub fn uninit(
        storage: S,
        storage_range: Range<u32>,
        data_buffer: &'s mut [u8; DATA_BUF_LEN],
    ) -> UninitializedSettings<'s, DATA_BUF_LEN, S> {
        defmt::debug!(
            "Settings::new() in range: {:x}, len: {}, data_buffer len: {}",
            storage_range,
            storage_range.end - storage_range.start,
            data_buffer.len()
        );
        UninitializedSettings {
            inner: Self {
                storage,
                storage_range,
                data_buffer,
                cache: NoCache::new(),
            },
        }
    }

    async fn verify_load(
        &mut self,
    ) -> Result<(), SettingsError<sequential_storage::Error<S::Error>>> {
        match sequential_storage::map::fetch_item::<SettingKey, &[u8], _>(
            &mut self.storage,
            self.storage_range.clone(),
            &mut self.cache,
            self.data_buffer,
            &0u32,
        )
        .await
        {
            Ok(Some(val)) if val == DATA_FORMAT_STRING.as_bytes() => Ok(()),
            Ok(Some(_)) => Err(SettingsError::CorruptOrInvalid),
            Ok(None) => Err(SettingsError::NotFound),
            Err(sequential_storage::Error::Corrupted {}) => Err(SettingsError::CorruptOrInvalid),
            Err(e) => Err(SettingsError::InnerError(e)),
        }
    }

    /// Get the value of a given setting
    pub async fn get<'v, V: Value<'v>>(
        &'v mut self,
        setting_key: &str,
    ) -> Result<Option<V>, SettingsError<sequential_storage::Error<S::Error>>> {
        sequential_storage::map::fetch_item(
            &mut self.storage,
            self.storage_range.clone(),
            &mut self.cache,
            self.data_buffer,
            &hash_key(setting_key),
        )
        .await
        .map_err(|e| e.into())
    }

    /// Get the value of a given setting and try to convert it to a string
    pub async fn get_str<'v>(
        &'v mut self,
        setting_key: &str,
    ) -> Result<Option<&'v str>, SettingsError<sequential_storage::Error<S::Error>>> {
        match self.get(setting_key).await {
            Ok(Some(bytes)) => match str::from_utf8(bytes) {
                Ok(str) => Ok(Some(str)),
                Err(e) => Err(SettingsError::StrConversionError(e)),
            },
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Get the value of a given setting and try to decode it into a given type
    ///
    /// This is a convenience method for fetching setting values that implement [`minicbor::Decode`].
    pub async fn get_decoded<'v, P: minicbor::Decode<'v, ()>>(
        &'v mut self,
        setting_key: &str,
    ) -> Result<Option<P>, SettingsError<sequential_storage::Error<S::Error>>> {
        match self.get(setting_key).await {
            Ok(Some(val)) => match minicbor::decode(val) {
                Ok(val) => Ok(Some(val)),
                Err(e) => Err(SettingsError::DecodeError(e)),
            },
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Get the value of the given setting and try to deserialize it into a given type
    ///
    /// This is a convenience method for fetching setting values that implement [`serde::Deserialize`].
    pub async fn get_deserialized<'v, P: serde::Deserialize<'v>>(
        &'v mut self,
        setting_key: &str,
    ) -> Result<Option<P>, SettingsError<sequential_storage::Error<S::Error>>> {
        match self.get(setting_key).await {
            Ok(Some(val)) => match minicbor_serde::from_slice(val) {
                Ok(val) => Ok(Some(val)),
                Err(e) => Err(SettingsError::DeserializeError(e)),
            },
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Overwrite the value of a given setting
    pub async fn set<'v, V: Value<'v>>(
        &mut self,
        setting_key: &str,
        value: &'v V,
    ) -> Result<(), SettingsError<sequential_storage::Error<S::Error>>> {
        sequential_storage::map::store_item(
            &mut self.storage,
            self.storage_range.clone(),
            &mut self.cache,
            self.data_buffer,
            &hash_key(setting_key),
            value,
        )
        .await
        .map_err(|e| e.into())
    }

    /// Overwrite a value of a given setting with any data that implements [`serde::Serialize`]
    pub async fn set_serialized<D: serde::Serialize>(
        &mut self,
        setting_key: &str,
        data: &D,
        buf: &mut [u8],
    ) -> Result<(), SettingsError<sequential_storage::Error<S::Error>>> {
        let mut ser = minicbor_serde::Serializer::new(BufWriter::new(buf));
        data.serialize(&mut ser)
            .map_err(|e| SettingsError::SerializeError(e))?;
        let len = ser.encoder().writer().written_len;
        let val: &[u8] = &buf[..len];
        sequential_storage::map::store_item::<_, &[u8], _>(
            &mut self.storage,
            self.storage_range.clone(),
            &mut self.cache,
            self.data_buffer,
            &hash_key(setting_key),
            &val,
        )
        .await
        .map_err(|e| e.into())
    }

    /// Blocking version of [`get()`]
    pub fn get_blocking<'v, V: Value<'v>>(
        &'v mut self,
        setting_key: &str,
    ) -> Result<Option<V>, SettingsError<sequential_storage::Error<S::Error>>> {
        let fut = self.get(setting_key);
        embassy_futures::block_on(fut)
    }

    /// Blocking version of [`get_str()`]
    pub fn get_str_blocking<'v>(
        &'v mut self,
        setting_key: &str,
    ) -> Result<Option<&'v str>, SettingsError<sequential_storage::Error<S::Error>>> {
        let fut = self.get_str(setting_key);
        embassy_futures::block_on(fut)
    }

    /// Blocking version of [`get_decoded()`]
    pub fn get_decoded_blocking<'v, P: minicbor::Decode<'v, ()>>(
        &'v mut self,
        setting_key: &str,
    ) -> Result<Option<P>, SettingsError<sequential_storage::Error<S::Error>>> {
        let fut = self.get_decoded(setting_key);
        embassy_futures::block_on(fut)
    }

    /// Blocking version of [`get_deserialized()`]
    pub fn get_deserialized_blocking<'v, P: serde::Deserialize<'v>>(
        &'v mut self,
        setting_key: &str,
    ) -> Result<Option<P>, SettingsError<sequential_storage::Error<S::Error>>> {
        let fut = self.get_deserialized(setting_key);
        embassy_futures::block_on(fut)
    }

    /// Blocking version of [`set()`]
    pub fn set_blocking<'v, V: Value<'v>>(
        &'v mut self,
        setting_key: &str,
        value: &'v V,
    ) -> Result<(), SettingsError<sequential_storage::Error<S::Error>>> {
        let fut = self.set(setting_key, value);
        embassy_futures::block_on(fut)
    }
}

pub fn hash_key(key: &str) -> SettingKey {
    use core::hash::Hasher as _;
    let mut hasher = fnv::FnvHasher::default();
    hasher.write(key.as_bytes());
    let result = hasher.finish();
    let result = u64::from_be(result) as SettingKey;
    defmt::trace!("hash_key(): {} -> {:x}", key, result);
    result
}

pub struct BufWriter<'b> {
    buf: &'b mut [u8],
    written_len: usize,
}

impl<'b> BufWriter<'b> {
    pub fn new(buf: &'b mut [u8]) -> Self {
        Self {
            buf,
            written_len: 0,
        }
    }
}
impl<'b> minicbor::encode::Write for BufWriter<'b> {
    type Error = EndOfSlice;

    fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        self.buf.write_all(buf)?;
        self.written_len += buf.len();
        Ok(())
    }
}
