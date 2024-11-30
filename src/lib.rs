#![no_std]

/// Wrapper around [`esp_hal::spi::master::SpiDmaBus`] to implement the [`embedded_hal_async::spi::SpiDevice`] trait.
pub struct EthSpi<'a, S: esp_hal::spi::master::Instance>(pub esp_hal::spi::master::SpiDmaBus<'a, esp_hal::Async, S>);
impl<'a, S: esp_hal::spi::master::Instance> embedded_hal_async::spi::ErrorType for EthSpi<'a, S> {
    type Error = esp_hal::spi::Error;
}

impl<'a, S: esp_hal::spi::master::Instance> embedded_hal_async::spi::SpiDevice for EthSpi<'a, S> {
    async fn transaction(
        &mut self,
        operations: &mut [embedded_hal_async::spi::Operation<'_, u8>],
    ) -> Result<(), Self::Error> {
        for op in operations {
            match op {
                embedded_hal_async::spi::Operation::Write(buf) => {
                    self.0.write_async(buf).await?;
                }
                embedded_hal_async::spi::Operation::Transfer(write_buf, read_buf) => {
                    self.0.transfer_async(write_buf, read_buf).await?;
                }
                embedded_hal_async::spi::Operation::Read(buf) => {
                    self.0.read_async(buf).await?;
                }
                embedded_hal_async::spi::Operation::TransferInPlace(buf) => {
                    self.0.transfer_in_place_async(buf).await?;
                }
                embedded_hal_async::spi::Operation::DelayNs(delay) => {
                    embassy_time::Timer::after_nanos(*delay as u64).await;
                }
            }
        }
        Ok(())
    }
}
