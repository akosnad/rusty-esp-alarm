#![feature(slice_split_once)]

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        mpsc::{self},
    },
    thread::JoinHandle,
};

use esp_idf_svc::hal::peripheral::Peripheral;
use esp_idf_svc::hal::spi::Dma;
use esp_idf_svc::hal::spi::SpiDriver;
use esp_idf_svc::hal::spi::SpiDriverConfig;
use esp_idf_svc::hal::{
    cpu::Core,
    gpio::{AnyIOPin, PinDriver},
    ledc::{LedcDriver, LedcTimerDriver, config::TimerConfig},
    peripherals::Peripherals,
    prelude::*,
    task::thread::ThreadSpawnConfiguration,
};

use esp_idf_svc::sys::esp_restart;
use esp_idf_svc::{
    eventloop::EspSystemEventLoop, nvs::EspDefaultNvsPartition, timer::EspTaskTimerService,
};
use ha_types::*;
use log::{error, info};
use scheduler::MqttSettings;
use seq_macro::seq;

mod alarm;
mod network;
mod scheduler;
mod settings;

use alarm::{AlarmCommand, AlarmEvent, AlarmState};

/// Helper which spawns a task with a name
fn spawn_task(
    task: impl FnOnce() + Send + 'static,
    task_name: &'static str,
    pin_to_core: Option<Core>,
) -> anyhow::Result<JoinHandle<()>> {
    info!("spawning task: {task_name}");

    ThreadSpawnConfiguration {
        name: Some(task_name.as_bytes()),
        pin_to_core,
        ..Default::default()
    }
    .set()?;

    let handle = std::thread::Builder::new().stack_size(8192).spawn(task)?;

    info!("spawned task: {task_name}");

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

#[allow(unreachable_code)]
fn main() -> anyhow::Result<()> {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    #[cfg(feature = "simulation")]
    {
        return simulation();
    }

    let peripherals = Peripherals::take()?;
    let mut pins = peripherals.pins;
    let sysloop = EspSystemEventLoop::take()?;
    let timer = EspTaskTimerService::new()?;

    // NVS API isn't actually used, but this call ensures
    // that the flash memory driver is initialized.
    let _ = EspDefaultNvsPartition::take()?;

    // SAFETY:
    // - this function is called exatly once during the lifetime of the program
    // - this function is called after the flash is initialized
    let settings = Arc::new(Mutex::new(unsafe { settings::init() }));

    let led = {
        let timer = LedcTimerDriver::new(
            peripherals.ledc.timer0,
            &TimerConfig::default().frequency(25.kHz().into()),
        )?;
        let led = LedcDriver::new(peripherals.ledc.channel0, timer, pins.gpio2)?;
        Box::leak(Box::new(led))
    };
    led.set_duty(0)?;

    let mac_addr: [u8; 6] = settings
        .lock()
        .unwrap()
        .get_blocking("mac-address")
        .map_err(|e| anyhow::anyhow!("failed getting `mac-address` setting: {e:?}"))?
        .ok_or(anyhow::anyhow!(
            "`mac-address` is not defineed in settings, but is required"
        ))?;

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
            Some(&mac_addr),
            None,
            sysloop.clone(),
        )?,
    )?));

    let mut tasks = Vec::new();
    let alarm_event_queue = Arc::new(std::sync::Mutex::new(VecDeque::new()));

    // Alarm task
    let (alarm_command_tx, alarm_command_rx) = mpsc::channel::<alarm::AlarmCommand>();
    let _alarm_event_queue = alarm_event_queue.clone();

    let siren_pin_num: u8 = settings
        .lock()
        .unwrap()
        .get_blocking("siren-pin")
        .map_err(|e| anyhow::anyhow!("failed getting `siren-pin` setting: {e:?}"))?
        .ok_or(anyhow::anyhow!(
            "`siren-pin` is not defined in settings, but is required"
        ))?;
    // SAFETY: stealing of the given GPIO pin is reliant on what is configured in settings.
    // no pins should be specified twice in settings. it this holds, then no pins are overlapping.
    // this guarantees that stealing the pin is safe; only one instance is being used of given pin.
    let siren_pin: Option<AnyIOPin> = unsafe {
        gpio_pin_num_to_peripheral!(siren_pin_num, pins, 0, 2)
            .or(gpio_pin_num_to_peripheral!(siren_pin_num, pins, 3, 5))
            .or(gpio_pin_num_to_peripheral!(siren_pin_num, pins, 6, 18))
            .or(gpio_pin_num_to_peripheral!(siren_pin_num, pins, 21, 23))
            .or(gpio_pin_num_to_peripheral!(siren_pin_num, pins, 25, 26))
            .or(gpio_pin_num_to_peripheral!(siren_pin_num, pins, 27, 28))
            .or(gpio_pin_num_to_peripheral!(siren_pin_num, pins, 32, 33))
    };
    let siren_pin = siren_pin.ok_or(anyhow::anyhow!(
        "siren pin is not a valid GPIO pin number: {siren_pin_num}"
    ))?;
    let mut siren_pin = PinDriver::output(siren_pin)?;
    siren_pin.set_low()?;

    let motion_entities: Vec<HAEntity> = settings
        .lock()
        .unwrap()
        .get_deserialized_blocking("motion-entities")
        .map_err(|e| anyhow::anyhow!("failed getting `motion-entities` setting: {e:?}"))?
        .ok_or(anyhow::anyhow!(
            "`motion-entities` is not defined in settings, but is required"
        ))?;
    log::info!("loaded motion entities: {motion_entities:?}");

    let mut alarm_motion_entites = motion_entities
        .iter()
        .cloned()
        .filter_map(|entity| {
            let pin = match entity.gpio_pin {
                // SAFETY: clone_unchecked() calls are safe because
                // we guarantee that the offending GPIO pins are only used by
                // the alarm task throughout the lifetime of the program.
                Some(pin) => unsafe {
                    let pin: Option<AnyIOPin> = gpio_pin_num_to_peripheral!(pin, pins, 0, 2)
                        .or(gpio_pin_num_to_peripheral!(pin, pins, 3, 5))
                        .or(gpio_pin_num_to_peripheral!(pin, pins, 6, 18))
                        .or(gpio_pin_num_to_peripheral!(pin, pins, 21, 23))
                        .or(gpio_pin_num_to_peripheral!(pin, pins, 25, 26))
                        .or(gpio_pin_num_to_peripheral!(pin, pins, 27, 28))
                        .or(gpio_pin_num_to_peripheral!(pin, pins, 32, 33));
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

    let alarm_entity: HAEntity = settings
        .lock()
        .unwrap()
        .get_deserialized_blocking("alarm-entity")
        .map_err(|e| anyhow::anyhow!("failed getting `alarm-entity` setting: {e:?}"))?
        .ok_or(anyhow::anyhow!(
            "`alarm-entity` is not defined in settings, but is required"
        ))?;
    log::info!("loaded alarm entity: {alarm_entity:?}");

    let alarm_entity_clone = alarm_entity.clone();
    let alarm_settings_clone = settings.clone();
    tasks.push(spawn_task(
        move || {
            alarm::alarm_task(
                _alarm_event_queue,
                alarm_command_rx,
                &mut alarm_motion_entites,
                alarm_entity_clone,
                siren_pin,
                alarm_settings_clone,
            );
        },
        "alarm\0",
        Some(Core::Core1),
    )?);

    let availability_topic = String::from(
        settings
            .lock()
            .unwrap()
            .get_str_blocking("availability-topic")
            .map_err(|e| anyhow::anyhow!("failed getting `availability-topic` setting: {e:?}"))?
            .ok_or(anyhow::anyhow!(
                "`availability-topic` is not defined in settings, but is required"
            ))?,
    );
    let ota_topic = String::from(
        settings
            .lock()
            .unwrap()
            .get_str_blocking("ota-topic")
            .map_err(|e| anyhow::anyhow!("failed getting `ota-topic` setting: {e:?}"))?
            .ok_or(anyhow::anyhow!(
                "`ota-topic` is not defined in settings, but is required"
            ))?,
    );
    let settings_topic_prefix = String::from(
        settings
            .lock()
            .unwrap()
            .get_str_blocking("settings-topic-prefix")
            .map_err(|e| anyhow::anyhow!("failed getting `settings-topic-prefix` setting: {e:?}"))?
            .ok_or(anyhow::anyhow!(
                "`settings-topic-prefix` is not defined in settings, but is required"
            ))?,
    );

    // Scheduler task
    let (status_tx, status_rx) = mpsc::channel::<StatusEvent>();
    let status_tx_scheduler = status_tx.clone();
    let alarm_command_tx_scheduler = alarm_command_tx.clone();
    let alarm_event_queue_scheduler = alarm_event_queue.clone();
    let availability_topic_clone = availability_topic.clone();
    let ota_topic_clone = ota_topic.clone();
    let scheduler_settings_clone = settings.clone();
    tasks.push(spawn_task(
        move || {
            scheduler::scheduler_task(
                &motion_entities,
                alarm_entity,
                status_rx,
                status_tx_scheduler,
                alarm_event_queue_scheduler,
                alarm_command_tx_scheduler,
                MqttSettings {
                    availability_topic: &availability_topic_clone,
                    ota_topic: &ota_topic_clone,
                    settings_topic_prefix: &settings_topic_prefix,
                },
                scheduler_settings_clone,
            );
        },
        "scheduler\0",
        Some(Core::Core0),
    )?);

    let hostname = String::from(
        settings
            .lock()
            .unwrap()
            .get_str_blocking("hostname")
            .map_err(|e| anyhow::anyhow!("failed getting `hostname` setting: {e:?}"))?
            .ok_or(anyhow::anyhow!(
                "`hostname` is not defined in settings, but is required"
            ))?,
    );
    let mqtt_endpoint = String::from(
        settings
            .lock()
            .unwrap()
            .get_str_blocking("mqtt-endpoint")
            .map_err(|e| anyhow::anyhow!("failed getting `mqtt-endpoint` setting: {e:?}"))?
            .ok_or(anyhow::anyhow!(
                "`mqtt-endpoint` is not defined in settings, but is required"
            ))?,
    );

    // Network stack
    network::init(
        eth,
        sysloop.clone(),
        timer,
        status_tx.clone(),
        &mut tasks,
        &network::NetworkSettings {
            hostname,
            mqtt_endpoint,
            availability_topic,
            ota_topic,
        },
    )?;

    // Wait for tasks to exit
    for task in tasks {
        task.join().unwrap();
    }

    error!("All tasks have exited, restarting...");

    unsafe {
        esp_restart();
    }
    Ok(())
}

enum StatusEvent {
    EthConnected,
    EthDisconnected,
    MqttConnected(esp_idf_svc::mqtt::client::EspMqttClient<'static>),
    MqttReconnected,
    MqttDisconnected,
    MqttMessage(MqttMessage),
    MqttMessageRaw { topic: String, payload: Vec<u8> },
}

#[derive(Debug, Clone)]
struct MqttMessage {
    topic: String,
    payload: String,
}

#[cfg(feature = "simulation")]
fn simulation() -> anyhow::Result<()> {
    use std::sync::mpsc::channel;
    use std::thread;

    let peripherals = Peripherals::take()?;
    let mut pins = peripherals.pins;
    let nvs = EspDefaultNvsPartition::take()?;

    let (alarm_command_tx, alarm_command_rx) = channel();

    // generate some alarm commands
    spawn_task(
        move || loop {
            thread::sleep(std::time::Duration::from_secs(5));
            alarm_command_tx.send(AlarmCommand::Arm).unwrap();
            thread::sleep(std::time::Duration::from_secs(20));
            alarm_command_tx.send(AlarmCommand::Disarm).unwrap();
        },
        "alarm_command_generator\0",
        None,
    )?;

    let entities: Vec<HAEntity> = vec![];
    let alarm_entity = entities
        .iter()
        .find(|entity| entity.variant == HAEntityVariant::alarm_control_panel)
        .expect("Alarm entity not found")
        .clone();
    let mut motion_entites = entities
        .clone()
        .into_iter()
        .filter_map(|entity| {
            let pin: Option<AnyIOPin> = unsafe {
                match entity.gpio_pin {
                    Some(pin) => match pin {
                        0 => Some(pins.gpio0.clone_unchecked().into()),
                        _ => None,
                    },
                    None => None,
                }
            };
            if pin.is_none() {
                return None;
            }

            let mut pin_driver = PinDriver::input(pin.unwrap()).unwrap();
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

    let queue = Arc::new(std::sync::Mutex::new(VecDeque::new()));

    let alarm_event_queue = queue.clone();
    spawn_task(
        move || {
            alarm::alarm_task(
                alarm_event_queue,
                alarm_command_rx,
                nvs,
                &mut motion_entites,
                alarm_entity,
            );
        },
        "alarm\0",
        Some(Core::Core1),
    )?;

    loop {
        // empty the queue
        if let Ok(mut queue) = queue.try_lock() {
            if let Some(event) = queue.pop_front() {
                println!("Popped alarm event: {:?}", event);
            }
        }
        thread::sleep(std::time::Duration::from_secs(1));
    }
}
