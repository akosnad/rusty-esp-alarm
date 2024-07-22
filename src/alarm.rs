use esp_idf_hal::gpio::{InputMode, InputPin, OutputPin, PinDriver};
use esp_idf_svc::nvs::*;
use ha_types::*;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

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
    Triggered,
}

#[derive(Clone, PartialEq)]
pub enum AlarmCommand {
    Arm,
    Disarm,
    ManualTrigger,
}

pub fn alarm_task<T, MODE>(
    event_queue: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<AlarmEvent>>>,
    command_rx: Receiver<AlarmCommand>,
    _nvs_default_partition: EspDefaultNvsPartition,
    motion_entities: &mut [AlarmMotionEntity<T, MODE>],
    alarm_entity: HAEntity,
) -> !
where
    T: InputPin + OutputPin,
    MODE: InputMode,
{
    // TODO: state persistence with NVS
    //let nvs = EspNvs::new(nvs_default_partition, "alarm", true).unwrap();
    let mut alarm_state = AlarmState::Disarmed;

    // TODO: make these configurable
    const ARMING_TIMEOUT: Duration = Duration::from_secs(30);
    const TRIGGER_AFTER_ARM_THRESHOLD: Duration = Duration::from_secs(5);

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
                AlarmCommand::Disarm => {
                    alarm_state = AlarmState::Disarmed;
                }
                AlarmCommand::ManualTrigger => {
                    if let AlarmState::Armed(_) = alarm_state {
                        alarm_state = AlarmState::Triggered;
                    }
                }
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
            AlarmState::Armed(start) => {
                if start.elapsed() >= TRIGGER_AFTER_ARM_THRESHOLD && motion_detected {
                    alarm_state = AlarmState::Triggered;
                }
            }
            AlarmState::Triggered => {}
        }

        if last_state != alarm_state {
            log::info!("Alarm state changed: {:?}", alarm_state);
            let mut queue = event_queue.lock().unwrap();
            queue.push_back(AlarmEvent::AlarmStateChanged((
                alarm_entity.clone(),
                alarm_state.clone(),
            )));
        }

        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}
