use core::ops::Range;
use embedded_storage_async::nor_flash::NorFlash;
use sequential_storage::{cache::NoCache, map::Value};

type SettingKey = u32;
type SettingValue<'v> = &'v [u8];

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

        sequential_storage::map::store_item::<SettingKey, SettingValue, _>(
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
    InnerError(#[defmt(Debug2Format)] E),
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
        match sequential_storage::map::fetch_item::<SettingKey, SettingValue, _>(
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
    pub async fn get<'v>(
        &'v mut self,
        setting_key: &str,
    ) -> Result<Option<SettingValue<'v>>, SettingsError<sequential_storage::Error<S::Error>>> {
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

    /// Blocking version of [`get()`]
    pub fn get_blocking<'v>(
        &'v mut self,
        setting_key: &str,
    ) -> Result<Option<SettingValue<'v>>, SettingsError<sequential_storage::Error<S::Error>>> {
        let fut = self.get(setting_key);
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
