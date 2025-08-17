use esp_idf_svc::hal::gpio::{InputMode, InputPin, Output, OutputPin, PinDriver};
use ha_types::*;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum AlarmEvent {
    MotionDetected(HAEntity),
    MotionCleared(HAEntity),
    AlarmStateChanged((HAEntity, AlarmState)),
}

pub struct AlarmMotionEntity<'a, T, MODE>
where
    T: InputPin + OutputPin,
    MODE: InputMode,
{
    pub entity: HAEntity,
    pub pin_driver: PinDriver<'a, T, MODE>,
    pub motion: bool,
}

#[derive(Clone, PartialEq, Debug)]
pub enum AlarmState {
    Disarmed,
    Arming(Instant),
    Armed(Instant),
    Pending(Instant),
    Triggered,
}

#[derive(Clone)]
pub enum AlarmCommand {
    Arm,
    ArmInstantly,
    Disarm,
    ManualTrigger,
    Untrigger,
    UpdateSettings(AlarmSettings),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub enum PersistedAlarmState {
    Disarmed,
    Armed,
    Triggered,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct AlarmSettings {
    initial_state: PersistedAlarmState,
    arming_timeout: u16,
    pending_timeout: u16,
}
impl Default for AlarmSettings {
    fn default() -> Self {
        Self {
            initial_state: PersistedAlarmState::Disarmed,
            arming_timeout: 90,
            pending_timeout: 30,
        }
    }
}

impl From<PersistedAlarmState> for AlarmState {
    fn from(value: PersistedAlarmState) -> Self {
        match value {
            PersistedAlarmState::Disarmed => Self::Disarmed,
            PersistedAlarmState::Armed => Self::Armed(Instant::now()),
            PersistedAlarmState::Triggered => Self::Triggered,
        }
    }
}

pub fn alarm_task<T, MODE>(
    event_queue: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<AlarmEvent>>>,
    command_rx: Receiver<AlarmCommand>,
    motion_entities: &mut [AlarmMotionEntity<T, MODE>],
    alarm_entity: HAEntity,
    mut siren_pin: PinDriver<impl OutputPin, Output>,
    settings: Arc<Mutex<crate::settings::Settings>>,
) -> !
where
    T: InputPin + OutputPin,
    MODE: InputMode,
{
    let mut alarm_settings: AlarmSettings = settings
        .lock()
        .unwrap()
        .get_deserialized_blocking("alarm-settings")
        .map_err(|e| anyhow::anyhow!("failed getting `alarm-settings` setting: {e:?}"))
        .unwrap_or_default()
        .unwrap_or_default();
    log::info!("loaded alarm settings: {alarm_settings:?}");

    // FIXME: a VecDeque is not suitable for emitting alarm events.
    // We need a more sophisticated data structure that can handle
    // only emitting the latest motion detected event for a given entity.

    let mut alarm_state: AlarmState = match settings
        .lock()
        .unwrap()
        .get_deserialized_blocking::<PersistedAlarmState>("persisted-alarm-state")
    {
        Ok(Some(state)) => {
            log::info!("loaded persisted alarm state: {state:?}");
            state.into()
        }
        Ok(None) => {
            log::info!("no persisted alarm state found, using configured initial state");
            alarm_settings.initial_state.into()
        }
        _ => AlarmState::Disarmed,
    };

    event_queue
        .lock()
        .unwrap()
        .push_back(AlarmEvent::AlarmStateChanged((
            alarm_entity.clone(),
            alarm_state.clone(),
        )));

    loop {
        let mut motion_detected = false;
        for e in motion_entities.iter_mut() {
            let motion = e.pin_driver.is_high();
            if motion == e.motion {
                continue;
            }

            log::info!("Motion at {}: {}", e.entity.name, motion);
            e.motion = motion;
            let mut queue = event_queue.lock().unwrap();
            if motion {
                motion_detected = true;
                queue.push_back(AlarmEvent::MotionDetected(e.entity.clone()));
            } else {
                queue.push_back(AlarmEvent::MotionCleared(e.entity.clone()));
            }
        }

        let last_state = alarm_state.clone();

        match command_rx.try_recv() {
            Ok(command) => match command {
                AlarmCommand::Arm => {
                    if alarm_state == AlarmState::Disarmed {
                        alarm_state = AlarmState::Arming(Instant::now());
                    }
                }
                AlarmCommand::ArmInstantly => {
                    if alarm_state == AlarmState::Disarmed {
                        alarm_state = AlarmState::Armed(Instant::now());
                    }
                }
                AlarmCommand::Disarm => {
                    alarm_state = AlarmState::Disarmed;
                }
                AlarmCommand::ManualTrigger => {
                    if let AlarmState::Armed(_) = alarm_state {
                        alarm_state = AlarmState::Triggered;
                    }
                }
                AlarmCommand::Untrigger => match alarm_state {
                    AlarmState::Triggered | AlarmState::Pending(_) => {
                        alarm_state = AlarmState::Armed(Instant::now());
                    }
                    _ => {}
                },
                AlarmCommand::UpdateSettings(new_settings) => {
                    alarm_settings = new_settings;
                    if let Err(e) = settings.lock().unwrap().set_serialized_blocking(
                        "alarm-settings",
                        &alarm_settings,
                        &mut [0u8; 1024],
                    ) {
                        log::error!("failed to write new alarm settings: {e:?}");
                    }
                }
            },
            Err(e) => {
                if e == std::sync::mpsc::TryRecvError::Disconnected {
                    panic!("command_rx disconnected");
                }
            }
        }

        let new_persisted_state = match alarm_state {
            AlarmState::Disarmed => PersistedAlarmState::Disarmed,
            AlarmState::Arming(start) => {
                if start.elapsed() >= Duration::from_secs(alarm_settings.arming_timeout as u64) {
                    alarm_state = AlarmState::Armed(Instant::now());
                }
                PersistedAlarmState::Armed
            }
            AlarmState::Armed(_start) => {
                if motion_detected {
                    alarm_state = AlarmState::Pending(Instant::now());
                    PersistedAlarmState::Triggered
                } else {
                    PersistedAlarmState::Armed
                }
            }
            AlarmState::Pending(start) => {
                if start.elapsed() >= Duration::from_secs(alarm_settings.pending_timeout as u64) {
                    alarm_state = AlarmState::Triggered;
                }
                PersistedAlarmState::Triggered
            }
            AlarmState::Triggered => {
                siren_pin.set_high().unwrap_or_else(|e| {
                    log::error!("Failed to set siren pin high: {e:?}");
                });
                PersistedAlarmState::Triggered
            }
        };

        if alarm_state != AlarmState::Triggered {
            siren_pin.set_low().unwrap_or_else(|e| {
                log::error!("Failed to set siren pin low: {e:?}");
            });
        }

        if last_state != alarm_state {
            log::info!("Alarm state changed: {alarm_state:?}");

            if let Err(e) = settings.lock().unwrap().set_serialized_blocking(
                "persisted-alarm-state",
                &new_persisted_state,
                &mut [0u8; 1024],
            ) {
                log::error!("failed to persist alarm state: {e:?}")
            }

            event_queue
                .lock()
                .unwrap()
                .push_back(AlarmEvent::AlarmStateChanged((
                    alarm_entity.clone(),
                    alarm_state.clone(),
                )));
        }

        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}
