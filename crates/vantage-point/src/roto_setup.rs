//! Deterministic VP setup projection; Serial I/O belongs to midistage.
use crate::roto_midi::MidiButtonSetup;
use midistage_profiles::roto_palette::closest_index;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(crate) const SETUP: u8 = 6;

#[derive(Clone, Debug)]
pub(crate) struct Generated {
    pub content: Value,
    pub hash: String,
}

pub(crate) fn generate(
    pins: &Value,
    labels: &BTreeMap<String, String>,
    midi: &MidiButtonSetup,
) -> Generated {
    let switches: Vec<Value> = (0..8)
        .map(|i| {
            let pin = &pins["slots"][i];
            let address = pin["address"].as_str().filter(|v| !v.is_empty());
            let present = address.is_some() && pin["path"].as_str().is_some_and(|v| !v.is_empty());
            let slug = address.and_then(|v| v.rsplit('/').next()).unwrap_or("");
            let label = address
                .and_then(|v| labels.get(v))
                .map(String::as_str)
                .unwrap_or(slug);
            let name = if i == 7 {
                "MIC".to_owned()
            } else if present {
                control_label(label, slug, i)
            } else {
                format!("#{}", i + 1)
            };
            let rgb = if i == 7 {
                [255, 0, 0]
            } else if present {
                pins["colors"][i]
                    .as_str()
                    .and_then(parse_color)
                    .unwrap_or([16; 3])
            } else {
                [16; 3]
            };
            let color = closest_index(rgb[0], rgb[1], rgb[2]);
            let off = if i == 7 {
                [128, 0, 0]
            } else {
                rgb.map(|v| ((u16::from(v) + 2) / 4) as u8)
            };
            json!({"control":i, "mode":"cc7", "channel":midi.channel,
            "param":midi.buttons[i], "nrpn_address":0, "min":midi.release,
            "max":midi.press, "name":name, "color":color, "led_on":color,
            "led_off":closest_index(off[0], off[1], off[2]), "action":"push", "step_names":[]})
        })
        .collect();
    let content = json!({"name":"VP", "knobs":[], "switches":switches});
    let mut digest = Sha256::new();
    digest.update(b"vp-roto-setup-v1\0");
    digest.update([SETUP]);
    digest.update(serde_json::to_vec(&content).expect("setup contains only JSON values"));
    Generated {
        content,
        hash: digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    }
}

fn parse_color(value: &str) -> Option<[u8; 3]> {
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some([0, 2, 4].map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("validated hex")))
}

fn control_label(label: &str, slug: &str, slot: usize) -> String {
    [label, slug]
        .into_iter()
        .map(str::trim)
        .find(|name| !name.is_empty() && name.bytes().all(|b| (32..=126).contains(&b)))
        .map(|name| name.chars().take(12).collect())
        .unwrap_or_else(|| format!("#{}", slot + 1))
}

#[cfg(test)]
mod tests {
    // Task: mem_1Cg9QiQeVsk7KD3fcwhXqh
    use super::*;
    use crate::roto_midi::VP_ROTO_SETUP;
    use midistage_profiles::roto_palette::closest_index;
    use serde_json::json;

    fn pins() -> Value {
        json!({"slots":[{"path":"/vp", "address":"vp/roto-vp-setup"}, null, null, null, null, null, null,
            {"path":"/vp", "address":"vp/unused"}],
            "colors":["#44c2b5", "#639af5", "#b78aee", "#f0b45a", "#c4d655", "#ef7b72", "#6fcf7e", "#e47ac9"], "dimGain":0.25})
    }

    #[test]
    fn labels_are_printable_ascii_twelve_bytes_with_slug_then_slot_fallback() {
        assert_eq!(control_label("long-lane-name", "slug", 0), "long-lane-na");
        assert_eq!(control_label("日本語", "roto-vp-setup", 1), "roto-vp-setu");
        assert_eq!(control_label("日本語", "日本語", 2), "#3");
        assert_eq!(control_label("bad\0name", "valid", 3), "valid");
        assert_eq!(control_label("", "", 6), "#7");
    }

    #[test]
    fn projection_uses_pin_colors_and_keeps_mic_fixed_and_knobs_clear() {
        let setup = generate(&pins(), &BTreeMap::new(), &VP_ROTO_SETUP);
        assert_eq!(setup.content["name"], "VP");
        assert_eq!(setup.content["knobs"], json!([]));
        let switches = setup.content["switches"].as_array().unwrap();
        assert_eq!(switches.len(), 8);
        assert_eq!(switches[0]["name"], "roto-vp-setu");
        assert_eq!(switches[0]["color"], closest_index(0x44, 0xc2, 0xb5));
        assert_eq!(switches[1]["name"], "#2");
        assert_eq!(switches[1]["color"], closest_index(16, 16, 16));
        assert_eq!(switches[7]["name"], "MIC");
        assert_eq!(switches[7]["led_on"], closest_index(255, 0, 0));
        assert_eq!(switches[7]["led_off"], closest_index(128, 0, 0));
        for (i, switch) in switches.iter().enumerate() {
            assert_eq!(switch["control"], i);
            assert_eq!(switch["channel"], VP_ROTO_SETUP.channel);
            assert_eq!(switch["param"], VP_ROTO_SETUP.buttons[i]);
            assert_eq!(switch["min"], VP_ROTO_SETUP.release);
            assert_eq!(switch["max"], VP_ROTO_SETUP.press);
            assert_eq!(switch["action"], "push");
        }
    }

    #[test]
    fn projection_follows_the_input_contract_instead_of_duplicating_cc_constants() {
        let midi = MidiButtonSetup {
            channel: 9,
            buttons: [1, 2, 3, 4, 5, 6, 7, 8],
            press: 100,
            release: 2,
        };
        let setup = generate(&pins(), &BTreeMap::new(), &midi);
        let switch = &setup.content["switches"][3];
        assert_eq!(switch["param"], 4);
        assert_eq!(switch["channel"], 9);
        assert_eq!(switch["max"], 100);
        assert_eq!(switch["min"], 2);
    }

    #[test]
    fn hash_tracks_generated_content_not_unused_pin_eight_or_input_key_order() {
        let mut input = pins();
        let original = generate(&input, &BTreeMap::new(), &VP_ROTO_SETUP);
        assert_eq!(original.hash.len(), 64);
        assert_eq!(
            original.hash,
            generate(&input, &BTreeMap::new(), &VP_ROTO_SETUP).hash
        );
        input["slots"][7] = Value::Null;
        input["colors"][7] = json!("#ffffff");
        assert_eq!(
            original.hash,
            generate(&input, &BTreeMap::new(), &VP_ROTO_SETUP).hash
        );
        input["colors"][0] = json!("#ff0000");
        assert_ne!(
            original.hash,
            generate(&input, &BTreeMap::new(), &VP_ROTO_SETUP).hash
        );
        let labels = BTreeMap::from([("vp/roto-vp-setup".to_owned(), "Studio".to_owned())]);
        assert_eq!(
            generate(&input, &labels, &VP_ROTO_SETUP).content["switches"][0]["name"],
            "Studio"
        );
    }

    #[test]
    fn missing_pins_are_dim_numbered_slots_and_mic() {
        let content = generate(&Value::Null, &BTreeMap::new(), &VP_ROTO_SETUP).content;
        for i in 0..7 {
            assert_eq!(content["switches"][i]["name"], format!("#{}", i + 1));
            assert_eq!(content["switches"][i]["color"], closest_index(16, 16, 16));
        }
        assert_eq!(content["switches"][7]["name"], "MIC");
    }

    #[test]
    fn mic_off_uses_actual_dark_red_instead_of_quantizing_to_black() {
        use midistage_profiles::roto_palette::ROTO_PALETTE;
        let content = generate(&Value::Null, &BTreeMap::new(), &VP_ROTO_SETUP).content;
        let mic = &content["switches"][7];
        assert_eq!(
            ROTO_PALETTE[mic["led_on"].as_u64().unwrap() as usize],
            0xff0000
        );
        assert_eq!(
            ROTO_PALETTE[mic["led_off"].as_u64().unwrap() as usize],
            0x800000
        );
    }
}
