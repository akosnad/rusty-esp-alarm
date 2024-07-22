use esp_idf_hal::gpio::{InputMode, InputPin, OutputPin, PinDriver};
use esp_idf_svc::nvs::*;
use ha_types::*;

pub enum AlarmEvent {
    MotionDetected(HAEntity),
    MotionCleared(HAEntity),
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

pub fn alarm_task<T, MODE>(
    event_queue: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<AlarmEvent>>>,
    _nvs_default_partition: EspDefaultNvsPartition,
    motion_entities: &mut [AlarmMotionEntity<T, MODE>],
) -> !
where
    T: InputPin + OutputPin,
    MODE: InputMode,
{
    // TODO: state persistence with NVS
    //let nvs = EspNvs::new(nvs_default_partition, "alarm", true).unwrap();

    // FIXME: a VecDeque is not suitable for emitting alarm events.
    // We need a more sophisticated data structure that can handle
    // only emitting the latest motion detected event for a given entity.

    loop {
        for e in motion_entities.iter_mut() {
            let motion = e.pin_driver.is_high();
            if motion == e.motion {
                continue;
            }

            log::info!("Motion at {}: {}", e.entity.name, motion);
            e.motion = motion;
            let mut queue = event_queue.lock().unwrap();
            if motion {
                queue.push_back(AlarmEvent::MotionDetected(e.entity.clone()));
            } else {
                queue.push_back(AlarmEvent::MotionCleared(e.entity.clone()));
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}
