//! HID 设备枚举与只读读取（setupapi / hid / kernel32，零第三方依赖）。
//!
//! 只做两件事：列出当前所有 HID 接口路径；打开接口后读属性、读 caps、阻塞读报告。
//! 全程 GENERIC_READ，不写设备。

#![allow(non_snake_case)]

use std::ffi::c_void;

type Handle = *mut c_void;

const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Guid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

/// HID 设备接口类 GUID：{4d1e55b2-f16f-11cf-88cb-001111000030}
const GUID_DEVINTERFACE_HID: Guid = Guid {
    data1: 0x4D1E_55B2,
    data2: 0xF16F,
    data3: 0x11CF,
    data4: [0x88, 0xCB, 0x00, 0x11, 0x11, 0x00, 0x00, 0x30],
};

#[repr(C)]
struct SpDeviceInterfaceData {
    cb_size: u32,
    interface_class_guid: Guid,
    flags: u32,
    reserved: usize,
}

#[repr(C)]
struct HiddAttributes {
    size: u32,
    vendor_id: u16,
    product_id: u16,
    version_number: u16,
}

#[repr(C)]
struct HidpCaps {
    usage: u16,
    usage_page: u16,
    input_report_byte_length: u16,
    output_report_byte_length: u16,
    feature_report_byte_length: u16,
    reserved: [u16; 17],
    number_link_collection_nodes: u16,
    number_input_button_caps: u16,
    number_input_value_caps: u16,
    number_input_data_indices: u16,
    number_output_button_caps: u16,
    number_output_value_caps: u16,
    number_output_data_indices: u16,
    number_feature_button_caps: u16,
    number_feature_value_caps: u16,
    number_feature_data_indices: u16,
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateFileW(
        file_name: *const u16,
        desired_access: u32,
        share_mode: u32,
        security_attributes: *mut c_void,
        creation_disposition: u32,
        flags_and_attributes: u32,
        template_file: Handle,
    ) -> Handle;
    fn ReadFile(
        file: Handle,
        buffer: *mut u8,
        bytes_to_read: u32,
        bytes_read: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
}

#[link(name = "setupapi")]
extern "system" {
    fn SetupDiGetClassDevsW(
        class_guid: *const Guid,
        enumerator: *const u16,
        parent: usize,
        flags: u32,
    ) -> Handle;
    fn SetupDiEnumDeviceInterfaces(
        device_info_set: Handle,
        device_info_data: *mut c_void,
        interface_class_guid: *const Guid,
        member_index: u32,
        device_interface_data: *mut SpDeviceInterfaceData,
    ) -> i32;
    fn SetupDiGetDeviceInterfaceDetailW(
        device_info_set: Handle,
        device_interface_data: *mut SpDeviceInterfaceData,
        device_interface_detail_data: *mut u8,
        detail_data_size: u32,
        required_size: *mut u32,
        device_info_data: *mut c_void,
    ) -> i32;
    fn SetupDiDestroyDeviceInfoList(device_info_set: Handle) -> i32;
}

#[link(name = "hid")]
extern "system" {
    fn HidD_GetAttributes(device: Handle, attributes: *mut HiddAttributes) -> i32;
    fn HidD_GetPreparsedData(device: Handle, preparsed_data: *mut *mut c_void) -> i32;
    fn HidD_FreePreparsedData(preparsed_data: *mut c_void) -> i32;
    fn HidP_GetCaps(preparsed_data: *mut c_void, capabilities: *mut HidpCaps) -> i32;
    fn HidD_GetProductString(device: Handle, buffer: *mut u16, buffer_len: u32) -> i32;
    fn HidD_GetManufacturerString(device: Handle, buffer: *mut u16, buffer_len: u32) -> i32;
}

/// 一个 HID 设备接口（依赖路径区分；同一物理设备会有多个路径）。
#[derive(Debug, Clone)]
pub struct HidDeviceInfo {
    pub path: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub usage_page: u16,
    pub usage: u16,
    /// 含 Report ID 的输入报告长度；0 表示没读到。
    pub input_report_len: usize,
    pub collection: Option<u16>,
    pub product: String,
    pub manufacturer: String,
}

impl HidDeviceInfo {
    pub fn group_key(&self) -> String {
        format!("{:04X}:{:04X}", self.vendor_id, self.product_id)
    }

    /// 是否属于键鼠类接口（用于「自动模式」的判断）。
    pub fn is_kbms(&self) -> bool {
        let p = self.usage_page;
        let u = self.usage;
        (p == 0x0001 && matches!(u, 0x0001 | 0x0002 | 0x0004 | 0x0005 | 0x0006))
            || p == 0x000C
            || (0xFF00..=0xFFFF).contains(&p)
    }
}

fn wide(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

fn from_wide(units: &[u16]) -> String {
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
        .trim()
        .trim_matches('\u{0}')
        .to_string()
}

/// 从设备路径提取 `VID_xxxx` / `PID_xxxx` / `COLxx`。
fn parse_ids(path: &str) -> (Option<u16>, Option<u16>, Option<u16>) {
    let upper = path.to_ascii_uppercase();
    let mut vid = None;
    let mut pid = None;
    let mut col = None;
    for part in upper.split('#') {
        for seg in part.split('&') {
            if let Some(hex) = seg.strip_prefix("VID_") {
                vid = u16::from_str_radix(hex, 16).ok();
            } else if let Some(hex) = seg.strip_prefix("PID_") {
                pid = u16::from_str_radix(hex, 16).ok();
            } else if let Some(hex) = seg.strip_prefix("COL") {
                col = hex.parse::<u16>().ok();
            }
        }
    }
    (vid, pid, col)
}

/// 枚举当前在场的所有 HID 接口路径（不含幽灵记录）。
pub fn list_paths() -> Vec<String> {
    const DIGCF_PRESENT: u32 = 0x0000_0002;
    const DIGCF_DEVICEINTERFACE: u32 = 0x0000_0010;

    let mut out = Vec::new();
    let set = unsafe {
        SetupDiGetClassDevsW(
            &GUID_DEVINTERFACE_HID,
            std::ptr::null(),
            0,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };
    if set == INVALID_HANDLE_VALUE || set.is_null() {
        return out;
    }

    let mut index = 0u32;
    loop {
        let mut did = SpDeviceInterfaceData {
            cb_size: std::mem::size_of::<SpDeviceInterfaceData>() as u32,
            interface_class_guid: GUID_DEVINTERFACE_HID,
            flags: 0,
            reserved: 0,
        };
        let ok = unsafe {
            SetupDiEnumDeviceInterfaces(
                set,
                std::ptr::null_mut(),
                &GUID_DEVINTERFACE_HID,
                index,
                &mut did,
            )
        };
        if ok == 0 {
            break;
        }
        index += 1;

        let mut required = 0u32;
        unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set,
                &mut did,
                std::ptr::null_mut(),
                0,
                &mut required,
                std::ptr::null_mut(),
            );
        }
        if required == 0 {
            continue;
        }

        let mut buf = vec![0u8; required as usize];
        let cb_size: u32 = if cfg!(target_pointer_width = "64") {
            8
        } else {
            6
        };
        buf[0..4].copy_from_slice(&cb_size.to_le_bytes());

        let ok = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set,
                &mut did,
                buf.as_mut_ptr(),
                required,
                &mut required,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            continue;
        }

        // DevicePath 位于偏移 4（x64），UTF-16 到 0 结束。
        let units: Vec<u16> = {
            let raw = &buf[4..];
            let mut v = Vec::new();
            for chunk in raw.as_chunks::<2>().0 {
                let u = u16::from_ne_bytes(*chunk);
                if u == 0 {
                    break;
                }
                v.push(u);
            }
            v
        };
        if units.is_empty() {
            continue;
        }
        let path = from_wide(&units);
        if !path.is_empty() && !out.contains(&path) {
            out.push(path);
        }
    }

    unsafe {
        SetupDiDestroyDeviceInfoList(set);
    }
    out
}

/// 读取一个接口的 VID/PID/用法/报告长度/名称。打不开也返回路径里解析出的 VID/PID。
pub fn inspect(path: &str) -> Option<HidDeviceInfo> {
    let (vid, pid, col) = parse_ids(path);
    let (vendor_id, product_id) = match (vid, pid) {
        (Some(v), Some(p)) => (v, p),
        _ => return None,
    };

    let mut info = HidDeviceInfo {
        path: path.to_string(),
        vendor_id,
        product_id,
        usage_page: 0,
        usage: 0,
        input_report_len: 0,
        collection: col,
        product: String::new(),
        manufacturer: String::new(),
    };

    let h = match open(path) {
        Ok(h) => h,
        Err(_) => return Some(info),
    };

    unsafe {
        let mut attrs = HiddAttributes {
            size: std::mem::size_of::<HiddAttributes>() as u32,
            vendor_id: 0,
            product_id: 0,
            version_number: 0,
        };
        if HidD_GetAttributes(h.raw(), &mut attrs) != 0 {
            info.vendor_id = attrs.vendor_id;
            info.product_id = attrs.product_id;
        }

        let mut pp: *mut c_void = std::ptr::null_mut();
        if HidD_GetPreparsedData(h.raw(), &mut pp) != 0 && !pp.is_null() {
            let mut caps: HidpCaps = std::mem::zeroed();
            if HidP_GetCaps(pp, &mut caps) != 0 {
                info.usage_page = caps.usage_page;
                info.usage = caps.usage;
                info.input_report_len = caps.input_report_byte_length as usize;
            }
            HidD_FreePreparsedData(pp);
        }

        let mut buf = [0u16; 128];
        if HidD_GetProductString(h.raw(), buf.as_mut_ptr(), (buf.len() * 2) as u32) != 0 {
            info.product = from_wide(&buf);
        }
        let mut mbuf = [0u16; 128];
        if HidD_GetManufacturerString(h.raw(), mbuf.as_mut_ptr(), (mbuf.len() * 2) as u32) != 0 {
            info.manufacturer = from_wide(&mbuf);
        }
    }

    Some(info)
}

/// 已打开的 HID 句柄；Drop 时关闭。
pub struct DeviceHandle {
    h: Handle,
}

unsafe impl Send for DeviceHandle {}

impl DeviceHandle {
    fn raw(&self) -> Handle {
        self.h
    }

    /// 阻塞读一帧，返回实际字节数。设备休眠时该调用会一直阻塞。
    pub fn read_raw(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut read = 0u32;
        let ok = unsafe {
            ReadFile(
                self.h,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(read as usize)
    }
}

impl Drop for DeviceHandle {
    fn drop(&mut self) {
        if self.h != INVALID_HANDLE_VALUE && !self.h.is_null() {
            unsafe {
                crate::sys::CloseHandle(self.h as isize);
            }
        }
    }
}

/// 共享只读打开：GENERIC_READ + FILE_SHARE_READ|WRITE + OPEN_EXISTING。
pub fn open(path: &str) -> Result<DeviceHandle, String> {
    const GENERIC_READ: u32 = 0x8000_0000;
    const FILE_SHARE_READ_WRITE: u32 = 0x0000_0003;
    const OPEN_EXISTING: u32 = 3;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;

    let wpath = wide(path);
    let h = unsafe {
        CreateFileW(
            wpath.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ_WRITE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE || h.is_null() {
        return Err(format!("打开失败：{path}"));
    }
    Ok(DeviceHandle { h })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_real_paths() {
        let mouse = r"\\?\HID#VID_24AE&PID_14A7&MI_01&COL09#8&363B33B3&0&0008#{4d1e55b2-f16f-11cf-88cb-001111000030}";
        assert_eq!(parse_ids(mouse), (Some(0x24AE), Some(0x14A7), Some(9)));
    }

    #[test]
    fn kbms_classify() {
        let mut d = HidDeviceInfo {
            path: String::new(),
            vendor_id: 1,
            product_id: 2,
            usage_page: 1,
            usage: 2,
            input_report_len: 8,
            collection: None,
            product: String::new(),
            manufacturer: String::new(),
        };
        assert!(d.is_kbms());
        d.usage_page = 0x0B;
        d.usage = 1;
        assert!(!d.is_kbms());
    }
}
