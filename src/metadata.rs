use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct Metadata {
    #[serde(rename = "Payload")]
    pub payload: Payload,
}

#[derive(Deserialize, Debug)]
pub struct Payload {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "ExternalID", default)]
    pub external_id: String,
    #[serde(rename = "Subject", default)]
    pub subject: String,
    #[serde(rename = "Sender", default)]
    pub sender: Option<Person>,
    #[serde(rename = "ToList", default)]
    pub to_list: Vec<Person>,
    #[serde(rename = "CCList", default)]
    pub cc_list: Vec<Person>,
    #[serde(rename = "LabelIDs", default)]
    pub label_ids: Vec<String>,
    #[serde(rename = "Time", default)]
    pub time: i64,
    #[serde(rename = "Size", default)]
    pub size: i64,
    #[serde(rename = "NumAttachments", default)]
    pub num_attachments: i64,
    #[serde(rename = "Attachments", default)]
    pub attachments: Vec<serde_json::Value>,
    #[serde(rename = "MIMEType", default)]
    pub mime_type: String,
}

#[derive(Deserialize, Debug, Clone, Default, serde::Serialize)]
pub struct Person {
    #[serde(rename = "Name", default)]
    pub name: String,
    #[serde(rename = "Address", default)]
    pub address: String,
}

pub fn parse_metadata(bytes: &[u8]) -> anyhow::Result<Metadata> {
    Ok(serde_json::from_slice(bytes)?)
}

/// labels.json formats seen:
///   - {"Version":1,"Payload":[{ID,Name,...},...]}  (Proton export v1)
///   - {"id": "name", ...} flat map
///   - [{ID, Name}, ...] bare array
pub fn parse_labels(bytes: &[u8]) -> Vec<(String, String)> {
    let value: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let value = value.get("Payload").cloned().unwrap_or(value);
    match &value {
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|item| {
                let id = item.get("ID")?.as_str()?.to_string();
                let name = item
                    .get("Name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default()
                    .to_string();
                Some((id, name))
            })
            .collect(),
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
            .collect(),
        _ => Vec::new(),
    }
}

/// Extract (filename, mime, size) from a Proton attachment object, defensively.
pub fn attachment_fields(v: &serde_json::Value) -> (String, String, i64) {
    let name = v
        .get("Name")
        .or_else(|| v.get("FileName"))
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();
    let mime = v
        .get("MIMEType")
        .or_else(|| v.get("ContentType"))
        .and_then(|x| x.as_str())
        .unwrap_or("application/octet-stream")
        .to_string();
    let size = v.get("Size").and_then(serde_json::Value::as_i64).unwrap_or(0);
    (name, mime, size)
}

/// Proton internal IDs are base64 and may contain '+', '/', '=' or start with
/// '-'. Sanitize to a path-safe directory component.
pub fn sanitize_id_component(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if cleaned.is_empty() {
        "_".to_string()
    } else {
        cleaned
    }
}

/// Sanitize an attachment filename for path construction: strip any directory
/// components, drop control characters, and reject empty/`.`/`..` results.
pub fn sanitize_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base.chars().filter(|c| !c.is_control()).collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        "attachment".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_metadata() {
        let json = br#"{"Version":1,"Payload":{"ID":"abc==","Subject":"Hi","Time":1700000000}}"#;
        let m = parse_metadata(json).unwrap();
        assert_eq!(m.payload.id, "abc==");
        assert_eq!(m.payload.subject, "Hi");
        assert_eq!(m.payload.time, 1_700_000_000);
    }

    #[test]
    fn parses_label_map_and_array() {
        let map = br#"{"5":"All Mail","15":"All Mail"}"#;
        assert_eq!(parse_labels(map).len(), 2);
        let arr = br#"[{"ID":"5","Name":"Inbox"}]"#;
        assert_eq!(parse_labels(arr), vec![("5".into(), "Inbox".into())]);
    }

    #[test]
    fn parses_proton_enveloped_labels() {
        let env = br#"{"Version":1,"Payload":[{"Path":"Inbox","ID":"0","Name":"Inbox"},{"Path":"Tax","ID":"kTax","Name":"Tax"}]}"#;
        let labels = parse_labels(env);
        assert_eq!(labels.len(), 2);
        assert!(labels.contains(&("kTax".into(), "Tax".into())));
    }

    #[test]
    fn sanitizes_hostile_ids() {
        assert_eq!(sanitize_id_component("-abc/1+2="), "_abc_1_2_");
        assert_eq!(sanitize_id_component(""), "_");
    }

    #[test]
    fn sanitizes_hostile_filenames() {
        assert_eq!(sanitize_filename("report.pdf"), "report.pdf");
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("C:\\evil\\x.exe"), "x.exe");
        assert_eq!(sanitize_filename(".."), "attachment");
        assert_eq!(sanitize_filename(""), "attachment");
        assert_eq!(sanitize_filename("a\u{0000}b"), "ab");
        assert_eq!(sanitize_filename("  spaced  "), "spaced");
    }
}
