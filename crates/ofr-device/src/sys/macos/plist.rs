//! `diskutil ... -plist` の出力から必要な値だけ拾う軽量リーダ。
//!
//! 完全な plist パーサではない。`<key>名前</key>` の直後に来る値だけを見る
//! 素朴な実装で、`diskutil info` の出力がほぼ平坦な dict であることに依存している。
//! 完全なパーサを持ち込むほどの用途ではないので、これで足りる範囲に留める。

/// `<key>key</key>` の直後の `<string>` の中身。
pub(super) fn string<'a>(xml: &'a str, key: &str) -> Option<&'a str> {
    let rest = after_key(xml, key)?;
    element(rest, "string")
}

/// `key` に対応する `<string>` を全て集める(ネストした dict の中も含む)。
pub(super) fn all_strings<'a>(xml: &'a str, key: &str) -> Vec<&'a str> {
    let needle = format!("<key>{key}</key>");
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(i) = xml[pos..].find(&needle) {
        let start = pos + i + needle.len();
        if let Some(v) = element(&xml[start..], "string") {
            out.push(v);
        }
        pos = start;
    }
    out
}

/// `<key>key</key>` の直後の `<integer>` の値。
pub(super) fn integer(xml: &str, key: &str) -> Option<u64> {
    let rest = after_key(xml, key)?;
    element(rest, "integer")?.trim().parse().ok()
}

/// `<key>key</key>` の直後の `<true/>` / `<false/>`。
pub(super) fn boolean(xml: &str, key: &str) -> Option<bool> {
    let rest = after_key(xml, key)?.trim_start();
    if rest.starts_with("<true/>") {
        Some(true)
    } else if rest.starts_with("<false/>") {
        Some(false)
    } else {
        None
    }
}

/// `<key>key</key>` の直後の `<array>` に入っている `<string>` の並び。
pub(super) fn string_array(xml: &str, key: &str) -> Vec<String> {
    let Some(rest) = after_key(xml, key) else {
        return Vec::new();
    };
    let Some(body) = element(rest, "array") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(i) = body[pos..].find("<string>") {
        let start = pos + i;
        match element(&body[start..], "string") {
            Some(v) => {
                out.push(v.to_string());
                pos = start + "<string>".len();
            }
            None => break,
        }
    }
    out
}

fn after_key<'a>(xml: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("<key>{key}</key>");
    let i = xml.find(&needle)?;
    Some(&xml[i + needle.len()..])
}

/// 先頭にある `<tag>...</tag>` の中身。先頭が別のタグなら `None`。
fn element<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let rest = xml.trim_start();
    let body = rest.strip_prefix(open.as_str())?;
    let end = body.find(close.as_str())?;
    Some(&body[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
	<key>DeviceIdentifier</key>
	<string>disk4</string>
	<key>DeviceBlockSize</key>
	<integer>512</integer>
	<key>Ejectable</key>
	<true/>
	<key>Internal</key>
	<false/>
	<key>MediaName</key>
	<string>Ultra USB 3.0 Media</string>
	<key>TotalSize</key>
	<integer>31029460992</integer>
	<key>APFSPhysicalStores</key>
	<array>
		<dict>
			<key>APFSPhysicalStore</key>
			<string>disk0s2</string>
		</dict>
	</array>
</dict>
</plist>
"#;

    #[test]
    fn reads_scalars() {
        assert_eq!(string(SAMPLE, "MediaName"), Some("Ultra USB 3.0 Media"));
        assert_eq!(integer(SAMPLE, "TotalSize"), Some(31029460992));
        assert_eq!(integer(SAMPLE, "DeviceBlockSize"), Some(512));
        assert_eq!(boolean(SAMPLE, "Ejectable"), Some(true));
        assert_eq!(boolean(SAMPLE, "Internal"), Some(false));
    }

    #[test]
    fn missing_keys_are_none() {
        assert_eq!(string(SAMPLE, "NoSuchKey"), None);
        assert_eq!(integer(SAMPLE, "NoSuchKey"), None);
        assert_eq!(boolean(SAMPLE, "NoSuchKey"), None);
        // 型が違えば None(整数キーを文字列として読もうとした場合)。
        assert_eq!(string(SAMPLE, "TotalSize"), None);
    }

    #[test]
    fn reads_nested_and_arrays() {
        assert_eq!(all_strings(SAMPLE, "APFSPhysicalStore"), vec!["disk0s2"]);

        let list = r#"<dict><key>WholeDisks</key><array>
            <string>disk0</string><string>disk2</string></array>
            <key>Other</key><array><string>ignored</string></array></dict>"#;
        assert_eq!(string_array(list, "WholeDisks"), vec!["disk0", "disk2"]);
        assert!(string_array(list, "Missing").is_empty());
    }
}
