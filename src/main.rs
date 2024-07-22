use std::{
    collections::VecDeque,
    sync::{
        mpsc::{self},
        Arc,
    },
    thread::JoinHandle,
};

use esp_idf_hal::peripheral::Peripheral;
use esp_idf_hal::{
    cpu::Core,
    gpio::{AnyIOPin, PinDriver},
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
    nvs::EspDefaultNvsPartition,
    timer::EspTaskTimerService,
};
use esp_idf_sys::{esp_restart, EspError};
use ha_types::*;
use log::{error, info};
use seq_macro::seq;

mod alarm;
mod network;
mod scheduler;

use alarm::{AlarmCommand, AlarmEvent, AlarmState};

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

macro_rules! gpio_pin_num_to_peripheral {
    ($pin:expr, $pins:ident, $from:expr, $to:expr) => { seq!(N in $from..$to {
        match $pin {
            #(
                N => Some($pins.gpio~N.clone_unchecked().into()),
            )*
                _ => None,
        }
    })};
}

fn main() -> anyhow::Result<()> {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = Peripherals::take()?;
    let mut pins = peripherals.pins;
    let sysloop = EspSystemEventLoop::take()?;
    let timer = EspTaskTimerService::new()?;
    let nvs = EspDefaultNvsPartition::take()?;

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

    let mut tasks = Vec::new();
    let alarm_event_queue = Arc::new(std::sync::Mutex::new(VecDeque::new()));

    // Alarm task
    let (alarm_command_tx, alarm_command_rx) = mpsc::channel::<alarm::AlarmCommand>();
    let _alarm_event_queue = alarm_event_queue.clone();
    let entities: Vec<HAEntity> = include!(concat!(env!("OUT_DIR"), "/entities.rs"));
    let mut motion_entites = entities
        .clone()
        .into_iter()
        .filter_map(|entity| {
            let pin = match entity.gpio_pin {
                // SAFETY: clone_unchecked() calls are safe because
                // we guarantee that the offending GPIO pins are only used by
                // the alarm task throughout the lifetime of the program.
                Some(pin) => unsafe {
                    let pin: Option<AnyIOPin> = gpio_pin_num_to_peripheral!(pin, pins, 0, 2)
                        .or_else(|| gpio_pin_num_to_peripheral!(pin, pins, 3, 5))
                        .or_else(|| gpio_pin_num_to_peripheral!(pin, pins, 6, 18))
                        .or_else(|| gpio_pin_num_to_peripheral!(pin, pins, 21, 23))
                        .or_else(|| gpio_pin_num_to_peripheral!(pin, pins, 25, 26))
                        .or_else(|| gpio_pin_num_to_peripheral!(pin, pins, 32, 33));
                    pin.expect("Invalid GPIO pin provided")
                },
                None => return None,
            };
            let mut pin_driver = PinDriver::input(pin).unwrap();
            pin_driver
                .set_pull(esp_idf_svc::hal::gpio::Pull::Up)
                .unwrap();

            Some(alarm::AlarmMotionEntity {
                entity,
                pin_driver,
                motion: false,
            })
        })
        .collect::<Vec<alarm::AlarmMotionEntity<_, _>>>();
    let alarm_entity = entities
        .iter()
        .find(|entity| entity.variant == HAEntityVariant::alarm_control_panel)
        .expect("Alarm entity not found")
        .clone();
    tasks.push(spawn_task(
        move || {
            alarm::alarm_task(
                _alarm_event_queue,
                alarm_command_rx,
                nvs,
                &mut motion_entites,
                alarm_entity,
            );
        },
        "alarm\0",
        Some(Core::Core1),
    )?);

    // Scheduler task
    let (status_tx, status_rx) = mpsc::channel::<StatusEvent>();
    let status_tx_scheduler = status_tx.clone();
    let alarm_command_tx_scheduler = alarm_command_tx.clone();
    let alarm_event_queue_scheduler = alarm_event_queue.clone();
    tasks.push(spawn_task(
        move || {
            scheduler::scheduler_task(
                &entities,
                status_rx,
                status_tx_scheduler,
                alarm_event_queue_scheduler,
                alarm_command_tx_scheduler,
            );
        },
        "scheduler\0",
        Some(Core::Core0),
    )?);

    // Network stack
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
    MqttMessage(MqttMessage),
}

#[derive(Debug, Clone)]
struct MqttMessage {
    topic: String,
    payload: String,
}
