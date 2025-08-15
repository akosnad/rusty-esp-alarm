const BUFFER_SIZE: usize = 4096;

/// Set up runtime settings storage
///
/// # Safety
/// - this function must be called at most once during the lifetime of the program
///   (there should only be one instance of Settings).
/// - It also must be called after `esp_flash_default_chip` is initialized. (this can be
///   guaranteed by esp-idf-svc after constructing an `EspNvsPartition`).
pub unsafe fn init() -> rusty_esp_alarm::settings::Settings<'static, BUFFER_SIZE, EspFlash> {
    // SAFETY: the calls to ESP-IDF bindings are used correctly.
    unsafe {
        let partition = esp_idf_svc::hal::sys::esp_partition_find_first(
            0x9E,
            esp_idf_svc::sys::esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_ANY,
            std::ptr::null(),
        );
        if partition.is_null() {
            panic!("no settings partition was found in partition table");
        }
        log::debug!("found settings partition: {:x?}", *partition);

        // match esp_idf_svc::hal::sys::esp_flash_init(std::ptr::null_mut()) {
        //     esp_idf_svc::hal::sys::ESP_OK => {}
        //     e_code => panic!(
        //         "failed to esp_flash_init() during settings initialization, error code: {e_code}"
        //     ),
        // };
        let data_buffer: &'static mut [u8; BUFFER_SIZE] = Box::leak(Box::new([0u8; BUFFER_SIZE]));
        let storage = EspFlash;
        let (partition_address, partition_size) = ((*partition).address, (*partition).size);
        let partition_range = partition_address..partition_address + partition_size;
        let uninit =
            rusty_esp_alarm::settings::Settings::uninit(storage, partition_range, data_buffer);
        uninit
            .init_blocking()
            .map_err(|e| panic!("failed to init settings: {e:?}"))
            .unwrap()
    }
}

pub struct EspFlash;

#[derive(Debug)]
pub enum EspFlashError {
    NotAligned,
    OutOfBounds,
    BadWrite,
    AlreadyInUse,
    OperationNotSupported,
    OperationOverlapsWithReadOnlyRegion,
    Other(i32),
    Unknown,
}
impl embedded_storage_async::nor_flash::NorFlashError for EspFlashError {
    fn kind(&self) -> embedded_storage_async::nor_flash::NorFlashErrorKind {
        match self {
            EspFlashError::NotAligned => {
                embedded_storage_async::nor_flash::NorFlashErrorKind::NotAligned
            }
            EspFlashError::OutOfBounds => {
                embedded_storage_async::nor_flash::NorFlashErrorKind::OutOfBounds
            }
            _ => embedded_storage_async::nor_flash::NorFlashErrorKind::Other,
        }
    }
}
impl embedded_storage_async::nor_flash::ErrorType for EspFlash {
    type Error = EspFlashError;
}
impl embedded_storage_async::nor_flash::ReadNorFlash for EspFlash {
    const READ_SIZE: usize = 4;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let buf = bytes.as_mut_ptr() as *mut std::ffi::c_void;
        // SAFETY: the call to the ESP-IDF binding is used correctly.
        // the buffer pointer is outlived by the mutable reference to the slice and the slice is not modified throughout.
        let result = unsafe {
            esp_idf_svc::hal::sys::esp_flash_read(
                std::ptr::null_mut(),
                buf,
                offset,
                bytes.len() as u32,
            )
        };
        match result {
            esp_idf_svc::hal::sys::ESP_OK => Ok(()),
            esp_idf_svc::hal::sys::ESP_ERR_NO_MEM => Err(EspFlashError::AlreadyInUse),
            other_err_code => Err(EspFlashError::Other(other_err_code)),
        }
    }

    fn capacity(&self) -> usize {
        // 4MB
        0x400_000
    }
}
impl embedded_storage_async::nor_flash::NorFlash for EspFlash {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = 0x1000;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let len = to - from;

        // SAFETY: the call to the ESP-IDF binding is used correctly.
        let result = unsafe {
            esp_idf_svc::hal::sys::esp_flash_erase_region(std::ptr::null_mut(), from, len)
        };
        match result {
            esp_idf_svc::hal::sys::ESP_OK => Ok(()),
            esp_idf_svc::hal::sys::ESP_ERR_NOT_SUPPORTED => {
                Err(EspFlashError::OperationNotSupported)
            }
            esp_idf_svc::hal::sys::ESP_ERR_NOT_ALLOWED => {
                Err(EspFlashError::OperationOverlapsWithReadOnlyRegion)
            }
            other_err_code => Err(EspFlashError::Other(other_err_code)),
        }
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let buf = bytes.as_ptr() as *const std::ffi::c_void;
        // SAFETY: the call to the ESP-IDF binding is used correctly.
        // the buffer pointer is outlived by the reference to the slice and the slice is not modified throughout.
        let result = unsafe {
            esp_idf_svc::hal::sys::esp_flash_write(
                std::ptr::null_mut(),
                buf,
                offset,
                bytes.len() as u32,
            )
        };
        match result {
            esp_idf_svc::hal::sys::ESP_OK => Ok(()),
            esp_idf_svc::hal::sys::ESP_FAIL => Err(EspFlashError::BadWrite),
            esp_idf_svc::hal::sys::ESP_ERR_NOT_SUPPORTED => {
                Err(EspFlashError::OperationNotSupported)
            }
            esp_idf_svc::hal::sys::ESP_ERR_NOT_ALLOWED => {
                Err(EspFlashError::OperationOverlapsWithReadOnlyRegion)
            }
            other_err_code => Err(EspFlashError::Other(other_err_code)),
        }
    }
}
