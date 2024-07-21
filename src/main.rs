use std::{
    collections::VecDeque,
    sync::{
        mpsc::{self},
        Arc,
    },
    thread::JoinHandle,
};

use esp_idf_hal::{
    cpu::Core,
    ledc::{config::TimerConfig, LedcDriver, LedcTimerDriver},
    peripherals::Peripherals,
    prelude::*,
    task::thread::ThreadSpawnConfiguration,
};
use esp_idf_svc::hal::spi::Dma;
use esp_idf_svc::hal::spi::SpiDriver;
use esp_idf_svc::hal::spi::SpiDriverConfig;

use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    mqtt::client::{ConnState, MessageImpl},
    timer::EspTaskTimerService,
};
use esp_idf_sys::{esp_restart, EspError};
use log::{error, info};

mod network;
mod scheduler;

/// Helper which spawns a task with a name
fn spawn_task(
    task: impl FnOnce() + Send + 'static,
    task_name: &'static str,
    pin_to_core: Option<Core>,
) -> anyhow::Result<JoinHandle<()>> {
    info!("spawning task: {}", task_name);

    ThreadSpawnConfiguration {
        name: Some(task_name.as_bytes()),
        pin_to_core,
        ..Default::default()
    }
    .set()?;

    let handle = std::thread::Builder::new().stack_size(4096).spawn(task)?;

    info!("spawned task: {}", task_name);

    Ok(handle)
}

fn main() -> anyhow::Result<()> {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = Peripherals::take()?;
    let pins = peripherals.pins;
    let sysloop = EspSystemEventLoop::take()?;
    let timer = EspTaskTimerService::new()?;

    let led = {
        let timer = LedcTimerDriver::new(
            peripherals.ledc.timer0,
            &TimerConfig::default().frequency(25.kHz().into()),
        )?;
        let led = LedcDriver::new(peripherals.ledc.channel0, timer, pins.gpio2)?;
        Box::leak(Box::new(led))
    };
    led.set_duty(0)?;

    let eth = Box::leak(Box::new(esp_idf_svc::eth::EspEth::wrap(
        esp_idf_svc::eth::EthDriver::new_spi(
            SpiDriver::new(
                peripherals.spi2,
                pins.gpio18,
                pins.gpio19,
                Some(pins.gpio23),
                &SpiDriverConfig::new().dma(Dma::Auto(4096)),
            )?,
            pins.gpio26,
            Some(pins.gpio5),
            Some(pins.gpio33),
            esp_idf_svc::eth::SpiEthChipset::W5500,
            20.MHz().into(),
            Some(&[0x02, 0x00, 0x00, 0xfc, 0x18, 0x01]),
            None,
            sysloop.clone(),
        )?,
    )?));

    // let mut pin_driver = esp_idf_svc::hal::gpio::PinDriver::input(pins.gpio4)?;
    // pin_driver.set_pull(esp_idf_svc::hal::gpio::Pull::Up)?;

    // loop {
    //     let motion = pin_driver.is_high();
    //     info!("Motion: {}", motion);
    //     std::thread::sleep(std::time::Duration::from_millis(250));
    // }
    //
    //

    let mut tasks = Vec::new();
    let alarm_event_queue = Arc::new(std::sync::Mutex::new(VecDeque::new()));

    // Scheduler task
    let (status_tx, status_rx) = mpsc::channel::<StatusEvent>();
    let status_tx_scheduler = status_tx.clone();
    let alarm_event_queue_scheduler = alarm_event_queue.clone();
    info!("Starting Scheduler...");
    tasks.push(spawn_task(
        move || {
            scheduler::scheduler_task(status_rx, status_tx_scheduler, alarm_event_queue_scheduler);
        },
        "scheduler\0",
        Some(Core::Core0),
    )?);

    // Network stack
    info!("Starting network stack...");
    network::init(eth, sysloop.clone(), timer, status_tx.clone(), &mut tasks)?;

    // Wait for tasks to exit
    for task in tasks {
        task.join().unwrap();
    }

    error!("All tasks have exited, restarting...");

    unsafe {
        esp_restart();
    }
}

enum StatusEvent {
    EthConnected,
    EthDisconnected,
    MqttConnected(
        esp_idf_svc::mqtt::client::EspMqttClient<'static, ConnState<MessageImpl, EspError>>,
    ),
    MqttReconnected,
    MqttDisconnected,
}
