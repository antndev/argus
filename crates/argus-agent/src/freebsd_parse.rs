use std::collections::HashMap;

use argus_model::Smart;

pub fn cpu_temperature(raw: &[u8]) -> Option<f64> {
    let text = std::str::from_utf8(raw).ok().and_then(|t| t.trim_end_matches('\0').trim().strip_suffix('C')?.parse().ok());
    text.or_else(|| <[u8; 4]>::try_from(raw).ok().map(|decikelvin| f64::from(i32::from_ne_bytes(decikelvin)) / 10.0 - 273.15))
}

pub fn nvmecontrol_health(device: &str, text: &str) -> Smart {
    let mut smart = Smart { device: device.to_string(), ..Default::default() };
    for line in text.lines().filter(|line| !line.starts_with(char::is_whitespace)) {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "Critical Warning State" => smart.passed = u8::from_str_radix(value.trim_start_matches("0x"), 16).is_ok_and(|w| w == 0),
            "Temperature" => {
                smart.temperature = value.split(',').find_map(|part| part.trim().strip_suffix(" C")?.trim().parse().ok());
            }
            "Percentage used" => smart.percent_used = value.parse().ok(),
            "Power on hours" => smart.power_on_hours = value.parse().ok(),
            "Media errors" => smart.media_errors = value.parse().ok(),
            _ => {}
        }
    }
    smart
}

pub fn interface_descriptions(xml: &str) -> HashMap<String, String> {
    let mut descriptions = HashMap::new();
    let Some(start) = xml.find("<interfaces>") else {
        return descriptions;
    };
    let mut rest = &xml[start + "<interfaces>".len()..];
    while let Some(open) = rest.find('<') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find('>') else { break };
        let tag = &rest[..close];
        if tag.starts_with('/') {
            break;
        }
        if tag.ends_with('/') || tag.starts_with('!') || tag.starts_with('?') {
            rest = &rest[close + 1..];
            continue;
        }
        let name = tag.split_whitespace().next().unwrap_or(tag);
        let end_tag = format!("</{name}>");
        let Some(end) = rest.find(&end_tag) else {
            break;
        };
        let body = &rest[close + 1..end];
        if let Some(device) = element(body, "if") {
            let description = element(body, "descr").filter(|d| !d.is_empty()).unwrap_or_else(|| name.to_uppercase());
            descriptions.insert(device, description);
        }
        rest = &rest[end + end_tag.len()..];
    }
    descriptions
}

fn element(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&format!("</{name}>"))?;
    Some(body[start..start + end].trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_temperature_int_and_string() {
        let value = cpu_temperature(&3181i32.to_ne_bytes()).unwrap_or_default();
        assert!((value - 44.95).abs() < 0.001);
        assert_eq!(cpu_temperature(b"45.0C\0"), Some(45.0));
        assert_eq!(cpu_temperature(b"warm!"), None);
    }

    #[test]
    fn nvmecontrol_output() {
        let text = "SMART/Health Information Log
============================
Critical Warning State:         0x00
 Available spare:               0
 Temperature:                   0
 Device reliability:            0
 Read only:                     0
 Volatile memory backup:        0
Temperature:                    318 K, 44.85 C, 112.73 F
Available spare:                100
Available spare threshold:      10
Percentage used:                3
Data units (512,000 byte) read: 1234567
Power on hours:                 8760
Unsafe shutdowns:               12
Media errors:                   0
Temperature Sensor 1:           320 K, 46.85 C, 116.33 F
";
        let smart = nvmecontrol_health("nvme0", text);
        assert!(smart.passed);
        assert_eq!(smart.temperature, Some(44.85));
        assert_eq!(smart.percent_used, Some(3.0));
        assert_eq!(smart.power_on_hours, Some(8760));
        assert_eq!(smart.media_errors, Some(0));
        assert!(!nvmecontrol_health("nvme0", "Critical Warning State:         0x04\n").passed);
    }

    #[test]
    fn opnsense_interfaces() {
        let xml = "<?xml version=\"1.0\"?>
<opnsense>
  <system><descr>not this</descr></system>
  <interfaces>
    <wan>
      <if>pppoe0</if>
      <descr/>
      <enable>1</enable>
    </wan>
    <lan>
      <enable>1</enable>
      <if>vlan020</if>
      <descr>DEFAULT</descr>
    </lan>
    <!-- note -->
    <opt2 type=\"x\">
      <if>vlan030</if>
      <descr>SERVICES</descr>
    </opt2>
    <openvpn>
      <descr>OpenVPN</descr>
    </openvpn>
    <opt3/>
  </interfaces>
  <opt9><if>igc9</if></opt9>
</opnsense>";
        let descriptions = interface_descriptions(xml);
        assert_eq!(descriptions.len(), 3);
        assert_eq!(descriptions["pppoe0"], "WAN");
        assert_eq!(descriptions["vlan020"], "DEFAULT");
        assert_eq!(descriptions["vlan030"], "SERVICES");
    }
}
