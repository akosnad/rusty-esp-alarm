use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::{
        ledc::{config::TimerConfig, LedcDriver, LedcTimerDriver},
        peripherals::Peripherals,
        prelude::*,
        spi,
    },
};

mod eth;
mod mqtt;

fn main() -> anyhow::Result<()> {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = Peripherals::take()?;
    let pins = peripherals.pins;
    let sysloop = EspSystemEventLoop::take()?;

    let led = {
        let timer = LedcTimerDriver::new(
            peripherals.ledc.timer0,
            &TimerConfig::default().frequency(25.kHz().into()),
        )?;
        let led = LedcDriver::new(peripherals.ledc.channel0, timer, pins.gpio2)?;
        Box::leak(Box::new(led))
    };
    led.set_duty(0)?;

    // let mut pin_driver = esp_idf_svc::hal::gpio::PinDriver::input(pins.gpio4)?;
    // pin_driver.set_pull(esp_idf_svc::hal::gpio::Pull::Up)?;

    // loop {
    //     let motion = pin_driver.is_high();
    //     info!("Motion: {}", motion);
    //     std::thread::sleep(std::time::Duration::from_millis(250));
    // }
    //

    let (status_tx, status_rx) = std::sync::mpsc::channel::<StatusEvent>();

    let eth = Box::leak(Box::new(esp_idf_svc::eth::EspEth::wrap(
        esp_idf_svc::eth::EthDriver::new_spi(
            spi::SpiDriver::new(
                peripherals.spi2,
                pins.gpio18,
                pins.gpio19,
                Some(pins.gpio23),
                &spi::SpiDriverConfig::new().dma(spi::Dma::Auto(4096)),
            )?,
            pins.gpio26,
            Some(pins.gpio5),
            Some(pins.gpio33),
            esp_idf_svc::eth::SpiEthChipset::W5500,
            20.MHz().into(),
            Some(&[0x02, 0x00, 0x00, 0x12, 0x34, 0x56]),
            None,
            sysloop.clone(),
        )?,
    )?));

    eth::init(sysloop.clone(), status_tx.clone(), eth)?;

    let mut eth_online = false;

    while let Ok(event) = status_rx.recv() {
        match event {
            StatusEvent::EthConnected => {
                eth_online = true;
                led.set_duty(30)?;
                mqtt::init(status_tx.clone())?;
            }
            StatusEvent::EthDisconnected => {
                eth_online = false;
                led.set_duty(0)?;
            }
            StatusEvent::MqttConnected => {
                led.set_duty(100)?;
            }
            StatusEvent::MqttDisconnected => {
                if eth_online {
                    led.set_duty(30)?;
                    mqtt::init(status_tx.clone())?;
                } else {
                    led.set_duty(0)?;
                }
            }
        }
    }

    Ok(())
}

enum StatusEvent {
    EthConnected,
    EthDisconnected,
    MqttConnected,
    MqttDisconnected,
}
