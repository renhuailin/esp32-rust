pub fn update_bit(old_value: u8, update_bits: u8, data: u8) -> u8 {
    let result = (old_value & (!update_bits)) | (update_bits & data);
    // println!(
    //     "old_value {}=0x{:X}={:08b}",
    //     old_value, old_value, old_value
    // );
    // println!(
    //     "update_bits {}=0x{:X}={:08b}",
    //     update_bits, update_bits, update_bits
    // );
    // println!("data {}=0x{:X}={:08b}", data, data, data);
    // println!("result {}=0x{:X}={:08b}", result, result, result);
    return result;
}
