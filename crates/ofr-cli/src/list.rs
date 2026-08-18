//! `ofr list`: デバイス一覧。

use ofr_device::{DeviceInfo, Result};

use crate::format::{self, display_width, pad, pad_left};

/// デバイスを列挙して表示する。
pub fn run(json: bool) -> Result<()> {
    let devices = ofr_device::list_devices()?;
    if json {
        print_json(&devices);
    } else {
        print_table(&devices);
    }
    Ok(())
}

fn print_table(devices: &[DeviceInfo]) {
    if devices.is_empty() {
        println!("デバイスが見つからない。");
        return;
    }

    let id_width = devices.iter().map(|d| d.id.len()).max().unwrap_or(2).max(2);
    let name_width = devices
        .iter()
        .map(|d| display_width(&d.display_name))
        .max()
        .unwrap_or(4)
        .max(4);

    const KIND_WIDTH: usize = 12;
    const SIZE_WIDTH: usize = 10;
    println!(
        "{}  {}  {}  {}  備考",
        pad("ID", id_width),
        pad("名前", name_width),
        pad_left("容量", SIZE_WIDTH),
        pad("種別", KIND_WIDTH),
    );
    for d in devices {
        let kind = if d.removable {
            "リムーバブル"
        } else {
            "内蔵"
        };
        let note = if d.is_system_disk {
            "起動ディスク(選択不可)"
        } else if !d.is_selectable_as_source() {
            "選択不可"
        } else {
            ""
        };
        println!(
            "{}  {}  {}  {}  {}",
            pad(&d.id, id_width),
            pad(&d.display_name, name_width),
            pad_left(&format::bytes(d.size_bytes), SIZE_WIDTH),
            pad(kind, KIND_WIDTH),
            note
        );
    }
    println!();
    println!("復旧元にできるのは「選択不可」以外のデバイス(PLAN.md 6章 3項)。");
    println!("生デバイスの読み込みには管理者 / root 権限が必要。");
}

fn print_json(devices: &[DeviceInfo]) {
    println!("[");
    for (i, d) in devices.iter().enumerate() {
        let comma = if i + 1 == devices.len() { "" } else { "," };
        println!(
            concat!(
                "  {{\"id\": {}, \"name\": {}, \"kind\": {}, \"size_bytes\": {}, ",
                "\"block_size\": {}, \"removable\": {}, \"system_disk\": {}, ",
                "\"selectable\": {}, \"serial\": {}}}{}"
            ),
            format::json_string(&d.id),
            format::json_string(&d.display_name),
            format::json_string(&d.kind.to_string()),
            d.size_bytes,
            d.block_size,
            d.removable,
            d.is_system_disk,
            d.is_selectable_as_source(),
            match &d.serial {
                Some(s) => format::json_string(s),
                None => "null".to_string(),
            },
            comma
        );
    }
    println!("]");
}
