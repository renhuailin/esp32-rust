use log::{error, info};

use crate::setting::nvs_setting::NvsSetting;

const OUTPUT_VOLUME: &str = "output_volume";
pub const DEFAULT_OUTPUT_VOLUME: u8 = 30;

pub fn load_volume_from_nvs() -> Result<u8, anyhow::Error> {
    let nvs = match NvsSetting::new("es8311") {
        std::result::Result::Ok(nvs) => nvs,
        Err(e) => {
            error!("failed to create nvs setting: {:?}", e);
            anyhow::bail!("failed to create nvs setting: {:?}", e)
        }
    };

    let wifi_setting_json = nvs.get_string(OUTPUT_VOLUME);

    if let Some(volume_str) = wifi_setting_json {
        let volume: u8 = volume_str.parse()?;
        info!("load volume from nvs - {}", volume);
        Ok(volume)
    } else {
        Ok(DEFAULT_OUTPUT_VOLUME)
    }
}

pub fn save_volume_to_nvs(volume: u8) -> Result<(), anyhow::Error> {
    let mut nvs = match NvsSetting::new("es8311") {
        std::result::Result::Ok(nvs) => nvs,
        Err(e) => {
            error!("failed to create nvs setting: {:?}", e);
            anyhow::bail!("failed to create nvs setting: {:?}", e)
        }
    };
    info!("save volume to nvs - {}", volume);
    nvs.set_string(OUTPUT_VOLUME, format!("{}", volume).as_str())?;
    Ok(())
}
