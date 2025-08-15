use esp_idf_svc::hal::gpio::{InputMode, InputPin, Output, OutputPin, PinDriver};
use ha_types::*;
use std::sync::mpsc::Receiver;
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

#[derive(Clone, PartialEq)]
pub enum AlarmCommand {
    Arm,
    ArmInstantly,
    Disarm,
    ManualTrigger,
    Untrigger,
}

pub fn alarm_task<T, MODE>(
    event_queue: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<AlarmEvent>>>,
    command_rx: Receiver<AlarmCommand>,
    motion_entities: &mut [AlarmMotionEntity<T, MODE>],
    alarm_entity: HAEntity,
    mut siren_pin: PinDriver<impl OutputPin, Output>,
) -> !
where
    T: InputPin + OutputPin,
    MODE: InputMode,
{
    // TODO: state persistence with runtime settings
    let mut alarm_state = AlarmState::Disarmed;

    // TODO: make these configurable
    const ARMING_TIMEOUT: Duration = Duration::from_secs(90);
    const PENDING_TIMEOUT: Duration = Duration::from_secs(30);

    // FIXME: a VecDeque is not suitable for emitting alarm events.
    // We need a more sophisticated data structure that can handle
    // only emitting the latest motion detected event for a given entity.

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
            },
            Err(e) => {
                if e == std::sync::mpsc::TryRecvError::Disconnected {
                    panic!("command_rx disconnected");
                }
            }
        }

        match alarm_state {
            AlarmState::Disarmed => {}
            AlarmState::Arming(start) => {
                if start.elapsed() >= ARMING_TIMEOUT {
                    alarm_state = AlarmState::Armed(Instant::now());
                }
            }
            AlarmState::Armed(_start) => {
                if motion_detected {
                    alarm_state = AlarmState::Pending(Instant::now());
                }
            }
            AlarmState::Pending(start) => {
                if start.elapsed() >= PENDING_TIMEOUT {
                    alarm_state = AlarmState::Triggered;
                }
            }
            AlarmState::Triggered => {
                siren_pin.set_high().unwrap_or_else(|e| {
                    log::error!("Failed to set siren pin high: {e:?}");
                });
            }
        }

        if last_state != alarm_state {
            log::info!("Alarm state changed: {alarm_state:?}");

            if last_state == AlarmState::Triggered {
                siren_pin.set_low().unwrap_or_else(|e| {
                    log::error!("Failed to set siren pin low: {e:?}");
                });
            }

            let mut queue = event_queue.lock().unwrap();
            queue.push_back(AlarmEvent::AlarmStateChanged((
                alarm_entity.clone(),
                alarm_state.clone(),
            )));
        }

        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}
