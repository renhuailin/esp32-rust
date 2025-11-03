use anyhow::Result;
use bytemuck::{self, cast_slice};

pub fn bytes_to_i16_slice(bytes: &[u8]) -> Result<&[i16]> {
    // bytemuck::cast_slice 会处理所有安全检查
    Ok(cast_slice::<u8, i16>(bytes))
}
