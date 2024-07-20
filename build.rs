use serde::Deserialize;

#[derive(Deserialize)]
struct Config {
    mqtt_endpoint: String,
    topic_prefix: String,
}
impl Config {
    fn verify(&self) -> anyhow::Result<()> {
        if self.mqtt_endpoint.is_empty() {
            anyhow::bail!("mqtt endpoint cannot be empty");
        }
        if !self.mqtt_endpoint.starts_with("mqtt://") {
            anyhow::bail!(
                "mqtt endpoint must start with \"mqtt://\". no other protocols are supported yet."
            );
        }
        Ok(())
    }
}

macro_rules! config_entry_to_env {
    ($config:ident, $env:ident, $entry:ident) => {
        println!("cargo:rustc-env={}={}", stringify!($env), $config.$entry);
    };
}

fn main() {
    embuild::espidf::sysenv::output();

    println!("cargo:rerun-if-changed=config.yml");

    let config_file = std::fs::read_to_string("config.yml").expect("config.yml not found");
    let config: Config = serde_yaml::from_str(&config_file).expect("config.yml is not valid yaml");
    config.verify().expect("config.yml validation failed");

    config_entry_to_env!(config, ESP_MQTT_ENDPOINT, mqtt_endpoint);
    config_entry_to_env!(config, ESP_TOPIC_PREFIX, topic_prefix);
}
