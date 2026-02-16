//! EDID parsing from xrandr output
//!
//! Parses the Extended Display Identification Data (EDID) blob
//! from `xrandr --verbose` output. Falls back gracefully if
//! xrandr is unavailable.

use std::process::Command;
use tracing::warn;

use super::host_info::EdidInfo;

/// Attempt to capture EDID info by running xrandr
pub fn capture_edid() -> Option<EdidInfo> {
    let output = match Command::new("xrandr").arg("--verbose").output() {
        Ok(output) => output,
        Err(_) => {
            warn!(
                "xrandr not found — EDID monitor info unavailable. \
                 Install xrandr for full monitor logging."
            );
            return None;
        }
    };

    if !output.status.success() {
        warn!("xrandr returned non-zero exit code — EDID unavailable");
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let hex = extract_edid_hex(&stdout)?;
    let bytes = hex_to_bytes(&hex)?;
    let mut edid = parse_edid_bytes(&bytes)?;
    edid.raw_hex = hex;

    // Try to extract model name from descriptor blocks
    if edid.model.is_none() {
        edid.model = extract_descriptor_string(&bytes, 0xFC); // Monitor name tag
    }
    if edid.serial.is_none() {
        edid.serial = extract_descriptor_string(&bytes, 0xFF); // Serial number tag
    }

    Some(edid)
}

/// Extract the EDID hex block from xrandr --verbose output
fn extract_edid_hex(xrandr_output: &str) -> Option<String> {
    let mut in_edid = false;
    let mut hex = String::new();

    for line in xrandr_output.lines() {
        let trimmed = line.trim();
        if trimmed == "EDID:" {
            in_edid = true;
            continue;
        }
        if in_edid {
            // EDID hex lines are indented and contain only hex chars
            if trimmed.chars().all(|c| c.is_ascii_hexdigit()) && !trimmed.is_empty() {
                hex.push_str(trimmed);
            } else {
                break;
            }
        }
    }

    if hex.is_empty() {
        None
    } else {
        Some(hex)
    }
}

/// Convert hex string to bytes
fn hex_to_bytes(hex: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let mut chars = hex.chars();
    while let (Some(hi), Some(lo)) = (chars.next(), chars.next()) {
        let byte = u8::from_str_radix(&format!("{}{}", hi, lo), 16).ok()?;
        bytes.push(byte);
    }
    Some(bytes)
}

/// Parse raw EDID bytes into EdidInfo
fn parse_edid_bytes(bytes: &[u8]) -> Option<EdidInfo> {
    if bytes.len() < 128 {
        return None;
    }

    // Validate EDID header: 00 FF FF FF FF FF FF 00
    let header = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
    if bytes[..8] != header {
        return None;
    }

    // Manufacturer ID (bytes 8-9): 3 letters encoded in 2 bytes
    let mfg_raw = ((bytes[8] as u16) << 8) | (bytes[9] as u16);
    let c1 = ((mfg_raw >> 10) & 0x1F) as u8 + b'A' - 1;
    let c2 = ((mfg_raw >> 5) & 0x1F) as u8 + b'A' - 1;
    let c3 = (mfg_raw & 0x1F) as u8 + b'A' - 1;
    let manufacturer =
        if c1.is_ascii_uppercase() && c2.is_ascii_uppercase() && c3.is_ascii_uppercase() {
            Some(format!("{}{}{}", c1 as char, c2 as char, c3 as char))
        } else {
            None
        };

    // Year (byte 17): year - 1990
    let year = if bytes[17] > 0 {
        Some(1990 + bytes[17] as u16)
    } else {
        None
    };

    // Gamma (byte 23): (gamma * 100) - 100, so gamma = (value + 100) / 100
    let gamma = if bytes[23] != 0xFF {
        Some((bytes[23] as f32 + 100.0) / 100.0)
    } else {
        None
    };

    Some(EdidInfo {
        raw_hex: String::new(), // Filled in by caller
        manufacturer,
        model: None,  // Filled from descriptor blocks
        serial: None, // Filled from descriptor blocks
        year,
        gamma,
    })
}

/// Extract a string from EDID descriptor blocks (bytes 54-125)
/// Each descriptor is 18 bytes. Tag byte is at offset 3.
fn extract_descriptor_string(bytes: &[u8], tag: u8) -> Option<String> {
    if bytes.len() < 126 {
        return None;
    }

    for desc_start in (54..=90).step_by(18) {
        // Check if this is a "display descriptor" (first two bytes are 0x00 0x00)
        if bytes[desc_start] == 0x00
            && bytes[desc_start + 1] == 0x00
            && bytes[desc_start + 3] == tag
        {
            // String data is in bytes 5-17 of the descriptor
            let str_bytes = &bytes[desc_start + 5..desc_start + 18];
            let s: String = str_bytes
                .iter()
                .take_while(|&&b| b != 0x0A && b != 0x00) // Terminated by newline or null
                .map(|&b| b as char)
                .collect();
            let trimmed = s.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_edid_block_valid() {
        let mut edid_bytes = vec![0u8; 128];
        // Header
        edid_bytes[0] = 0x00;
        edid_bytes[1] = 0xFF;
        edid_bytes[2] = 0xFF;
        edid_bytes[3] = 0xFF;
        edid_bytes[4] = 0xFF;
        edid_bytes[5] = 0xFF;
        edid_bytes[6] = 0xFF;
        edid_bytes[7] = 0x00;
        // Manufacturer: "DEL" (Dell) = 0x10AC
        edid_bytes[8] = 0x10;
        edid_bytes[9] = 0xAC;
        // Manufacture year: 2020 (byte 17 = year - 1990 = 30)
        edid_bytes[17] = 30;
        // Gamma: 2.2 = byte value 120 (gamma * 100 - 100)
        edid_bytes[23] = 120;

        let edid = parse_edid_bytes(&edid_bytes);
        assert!(edid.is_some());
        let edid = edid.unwrap();
        assert_eq!(edid.manufacturer.as_deref(), Some("DEL"));
        assert_eq!(edid.year, Some(2020));
        assert!((edid.gamma.unwrap() - 2.2).abs() < 0.01);
    }

    #[test]
    fn test_parse_edid_block_invalid_header() {
        let edid_bytes = vec![0u8; 128]; // All zeros, invalid header
        let edid = parse_edid_bytes(&edid_bytes);
        assert!(edid.is_none());
    }

    #[test]
    fn test_extract_edid_hex_from_xrandr() {
        let xrandr_output = r#"
Screen 0: minimum 8 x 8, current 3840 x 2160, maximum 32767 x 32767
DP-0 connected primary 3840x2160+0+0 (normal left inverted right x axis y axis) 600mm x 340mm
   3840x2160     60.00*+
        EDID:
                00ffffffffffff001e6d085b7c5b0000
                0b1e0104b53c22783aee95a3544c9926
                0f5054254b80714f81809500a9c0b300
                d1c0814001014dd000a0f0703e803020
                350055502100001a286800a0f0703e80
                0890350055502100001a000000fd0030
                901ee63c000a202020202020000000fc
                004c472048445220344b0a20200001e8
   1920x1080     60.00    59.94
"#;
        let hex = extract_edid_hex(xrandr_output);
        assert!(hex.is_some());
        let hex = hex.unwrap();
        assert!(hex.starts_with("00ffffffffffff00"));
    }

    #[test]
    fn test_extract_edid_hex_no_edid() {
        let xrandr_output = "DP-0 connected primary 3840x2160+0+0\n   3840x2160     60.00*+\n";
        let hex = extract_edid_hex(xrandr_output);
        assert!(hex.is_none());
    }
}
