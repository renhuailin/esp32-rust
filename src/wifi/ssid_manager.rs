use anyhow::{Ok, Result};
use chrono::{DateTime, Utc};
use esp_idf_svc::ping::Info;
use log::{error, info};
use serde::{Deserialize, Serialize};

use crate::setting::nvs_setting::NvsSetting;

#[derive(Debug, Serialize, Deserialize)]
pub struct SsidItem {
    pub ssid: String,
    pub password: String,
    pub last_connect_time: String,
}

pub struct SsidMananger {}

const MAX_SSID_COUNT: usize = 10;
const WIFI_SETTING_KEY: &str = "wifi_settings";
impl SsidMananger {
    pub fn get_instance() -> Self {
        Self {}
    }
    pub fn get_saved_ssid_list(&self) -> Result<Vec<SsidItem>> {
        let nvs = match NvsSetting::new("wifi") {
            std::result::Result::Ok(nvs) => nvs,
            Err(e) => {
                error!("failed to create nvs setting: {:?}", e);
                return Err(e.into());
            }
        };

        let mut ssid_items = Vec::new();

        info!("try to get wifi_setting_json");
        let wifi_setting_json = nvs.get_string(WIFI_SETTING_KEY);
        info!("wifi_setting_json: {:?}", wifi_setting_json);

        if let Some(json_string) = wifi_setting_json {
            match serde_json::from_str::<Vec<SsidItem>>(&json_string) {
                std::result::Result::Ok(mut items) => {
                    items.sort_by(|a, b| {
                        let a_time = DateTime::parse_from_rfc3339(&a.last_connect_time);
                        let b_time = DateTime::parse_from_rfc3339(&b.last_connect_time);
                        if a_time.is_ok() && b_time.is_ok() {
                            //最后连接时间倒序排序
                            b_time.unwrap().cmp(&a_time.unwrap())
                        } else {
                            std::cmp::Ordering::Equal
                        }
                    });
                    ssid_items = items;
                }
                Err(_) => {}
            }
        }

        Ok(ssid_items)
    }

    pub fn add_ssid(&mut self, ssid: &str, password: &str) -> Result<()> {
        let mut ssid_list = self.get_saved_ssid_list()?;
        if ssid_list.len() >= MAX_SSID_COUNT {
            // 把列表按最后连接时间升序排序, 删除最老的
            ssid_list.sort_by(|a, b| {
                let a_time = DateTime::parse_from_rfc3339(&a.last_connect_time);
                let b_time = DateTime::parse_from_rfc3339(&b.last_connect_time);
                if a_time.is_ok() && b_time.is_ok() {
                    a_time.unwrap().cmp(&b_time.unwrap())
                } else {
                    std::cmp::Ordering::Equal
                }
            });

            ssid_list.remove(0);
        }

        ssid_list.push(SsidItem {
            ssid: ssid.to_string(),
            password: password.to_string(),
            last_connect_time: Utc::now().to_rfc3339(),
        });
        self.save_to_nvs(&ssid_list)?;
        Ok(())
    }

    fn save_to_nvs(&mut self, ssid_list: &[SsidItem]) -> Result<()> {
        let mut nvs = NvsSetting::new("wifi")?;

        let json_string = serde_json::to_string(ssid_list)?;
        nvs.set_string(WIFI_SETTING_KEY, &json_string)?;

        Ok(())
    }

    pub fn update_ssid_item(&mut self, ssid_item: SsidItem) -> Result<()> {
        info!("try to update ssid_item: {:?}", ssid_item);
        let mut ssid_list = self.get_saved_ssid_list()?;
        for i in 0..ssid_list.len() {
            if ssid_list[i].ssid == ssid_item.ssid {
                ssid_list[i] = ssid_item;
                break;
            }
        }
        info!("try to save ssid_list to nvs: {:?}", ssid_list);
        self.save_to_nvs(&ssid_list)?;
        Ok(())
    }

    pub fn update_ssid_last_connect_time(
        &mut self,
        ssid: &str,
        last_connect_time: &str,
    ) -> anyhow::Result<()> {
        let saved_ssid_list = self.get_saved_ssid_list()?;
        for mut ssid_item in saved_ssid_list {
            if ssid_item.ssid == ssid {
                ssid_item.last_connect_time = last_connect_time.to_string();
                self.update_ssid_item(ssid_item)?;
                return Ok(());
            }
        }
        Ok(())
    }
}
