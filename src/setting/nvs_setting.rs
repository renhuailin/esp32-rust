use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, EspNvsPartition, NvsDefault};

///https://docs.esp-rs.org/esp-idf-svc/esp_idf_svc/nvs/struct.EspNvs.html
pub struct NvsSetting {
    nvs: EspNvs<NvsDefault>,
}

impl NvsSetting {
    pub fn new(namespace: &str) -> anyhow::Result<Self> {
        let nvs_default_partition: EspNvsPartition<NvsDefault> = EspDefaultNvsPartition::take()?;
        let nvs: EspNvs<NvsDefault> = EspNvs::new(nvs_default_partition, namespace, true)?;
        Ok(Self { nvs })
    }

    pub fn get_string(&self, key: &str) -> Option<String> {
        // String values are limited in the IDF to 4000 bytes, but our buffer is shorter.
        const MAX_STR_LEN: usize = 100;
        let mut buffer: [u8; MAX_STR_LEN] = [0; MAX_STR_LEN];
        match self.nvs.get_str(key, &mut buffer).unwrap() {
            Some(v) => {
                let value = Some(v.to_string());
                return value;
            }
            None => {
                return None;
            }
        };
    }

    pub fn set_string(&mut self, key: &str, value: &str) -> anyhow::Result<()> {
        self.nvs.set_str(key, value)?;
        Ok(())
    }

    pub fn get_i32(&self, key: &str) -> Option<i32> {
        match self.nvs.get_i32(key) {
            Ok(value) => value,
            Err(_) => None,
        }
    }

    pub fn set_i32(&mut self, key: &str, value: i32) -> anyhow::Result<()> {
        self.nvs.set_i32(key, value)?;
        Ok(())
    }

    pub fn get_u8(&self, key: &str) -> Option<u8> {
        match self.nvs.get_u8(key) {
            Ok(value) => value,
            Err(_) => None,
        }
    }

    pub fn set_u8(&mut self, key: &str, value: u8) -> anyhow::Result<()> {
        self.nvs.set_u8(key, value)?;
        Ok(())
    }
}
