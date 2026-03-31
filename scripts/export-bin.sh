SCRIPTPATH="$( cd "$(dirname "$0")" ; pwd -P )"

cd $SCRIPTPATH/..

pwd

# 1. 先进行 release 编译，确保开启了优化
cargo build --release

# 2. 导出 OTA 使用的二进制文件
espflash save-image --chip esp32s3  --flash-size 16mb  --partition-table partitions/v1/16m.csv target/xtensa-esp32s3-espidf/release/xiaoxin_esp32 /Users/harley/workspaces/Projects/xiaozhi/xiaozhi_ota_server/public/firmwares/firmware.bin