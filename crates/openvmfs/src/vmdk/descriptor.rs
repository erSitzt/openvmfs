//! The VMDK descriptor: the small text file that describes a virtual disk.
//!
//! Written from VMware's "Virtual Disk Format 5.0" technical note and from descriptors written by ESXi.
//! Format description: `docs/format/70-vmdk-descriptor.md`.

use std::collections::BTreeMap;

use crate::error::{Error, Result};

/// `parentCID` value of a disk that has no parent.
pub const NO_PARENT_CID: u32 = 0xFFFF_FFFF;

/// Who may access an extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Access {
    /// Read and write.
    ReadWrite,
    /// Read only.
    ReadOnly,
    /// No access.
    NoAccess,
}

/// The kind of an extent, which decides how its file is read.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ExtentType {
    /// `VMFS`: a flat file on a VMFS datastore. Thin or thick is a property of the file's block map.
    Vmfs,
    /// `VMFSSPARSE`: a redo log in the older COWD format.
    VmfsSparse,
    /// `SESPARSE`: a snapshot delta in the space-efficient sparse format, the one VMFS6 creates.
    SeSparse,
    /// `VMFSRDM`: a raw device mapping.
    VmfsRdm,
    /// `VMFSRAW`: a raw device.
    VmfsRaw,
    /// `FLAT`: a flat file of a hosted product.
    Flat,
    /// `SPARSE`: a sparse file of a hosted product.
    Sparse,
    /// `ZERO`: reads as zeros.
    Zero,
    /// Anything else, kept as written.
    Other(String),
}

impl ExtentType {
    fn parse(word: &str) -> ExtentType {
        match word.to_ascii_uppercase().as_str() {
            "VMFS" => ExtentType::Vmfs,
            "VMFSSPARSE" => ExtentType::VmfsSparse,
            "SESPARSE" => ExtentType::SeSparse,
            "VMFSRDM" => ExtentType::VmfsRdm,
            "VMFSRAW" => ExtentType::VmfsRaw,
            "FLAT" => ExtentType::Flat,
            "SPARSE" => ExtentType::Sparse,
            "ZERO" => ExtentType::Zero,
            _ => ExtentType::Other(word.to_owned()),
        }
    }
}

/// One extent line, for example `RW 4192256 VMFS "disk-flat.vmdk"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtentLine {
    /// Access mode.
    pub access: Access,
    /// Size in 512-byte sectors.
    pub sectors: u64,
    /// Kind of extent.
    pub kind: ExtentType,
    /// File name, relative to the descriptor unless it is an absolute path.
    pub file_name: String,
    /// Offset into the file in sectors, if given.
    pub offset: Option<u64>,
}

/// A parsed descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    /// Descriptor version. 3 means changed block tracking is enabled.
    pub version: u32,
    /// Content id.
    pub cid: u32,
    /// Content id of the parent, [`NO_PARENT_CID`] for a base disk.
    pub parent_cid: u32,
    /// The `createType` value as written, for example `vmfs` or `seSparse`.
    pub create_type: String,
    /// Path of the parent descriptor, for delta links.
    pub parent_file_name_hint: Option<String>,
    /// The extents. ESXi writes exactly one per descriptor.
    pub extents: Vec<ExtentLine>,
    /// All `key = value` entries with lower-cased keys, including the disk database (`ddb.*`).
    pub entries: BTreeMap<String, String>,
}

impl Descriptor {
    /// Parses the text of a descriptor file.
    pub fn parse(raw: &[u8]) -> Result<Descriptor> {
        // Some tools pad descriptors with NUL bytes.
        let end = raw.iter().rposition(|&b| b != 0).map_or(0, |i| i.saturating_add(1));
        let text = std::str::from_utf8(raw.get(..end).unwrap_or_default())
            .map_err(|_| Error::unsupported("the descriptor is not UTF-8 text (is this a binary .vmdk file?)"))?;

        let mut entries = BTreeMap::new();
        let mut extents = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let number = index.saturating_add(1);
            if let Some(extent) = parse_extent(line, number)? {
                extents.push(extent);
                continue;
            }
            let (key, value) = line.split_once('=').ok_or_else(|| {
                Error::corrupt(format!(
                    "descriptor line {number} is neither an extent nor a key = value entry: {line:?}"
                ))
            })?;
            let key = key.trim().to_ascii_lowercase();
            if key.is_empty() || key.contains(char::is_whitespace) {
                return Err(Error::corrupt(format!(
                    "descriptor line {number} has an invalid key: {line:?}"
                )));
            }
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(value);
            if entries.insert(key.clone(), value.to_owned()).is_some() {
                return Err(Error::corrupt(format!(
                    "descriptor line {number} repeats the key {key:?}"
                )));
            }
        }

        let required = |key: &str| -> Result<&String> {
            entries
                .get(key)
                .ok_or_else(|| Error::corrupt(format!("descriptor has no {key:?} entry")))
        };
        let hex = |key: &str| -> Result<u32> {
            let value = required(key)?;
            u32::from_str_radix(value, 16)
                .map_err(|_| Error::corrupt(format!("descriptor entry {key:?} is not a hexadecimal id: {value:?}")))
        };
        let version = required("version")?
            .parse::<u32>()
            .map_err(|_| Error::corrupt("descriptor entry \"version\" is not a number"))?;
        let descriptor = Descriptor {
            version,
            cid: hex("cid")?,
            parent_cid: hex("parentcid")?,
            create_type: required("createtype")?.clone(),
            parent_file_name_hint: entries.get("parentfilenamehint").cloned(),
            extents,
            entries,
        };
        Ok(descriptor)
    }

    /// Looks up an entry by its key, ignoring case: `ddb.thinProvisioned`, `encoding`, ...
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(&key.to_ascii_lowercase()).map(String::as_str)
    }

    /// Whether the descriptor says the disk is thin provisioned. Informational: allocation is a property of the
    /// flat file's block map, not of this flag.
    pub fn thin_provisioned(&self) -> bool {
        self.get("ddb.thinProvisioned") == Some("1")
    }

    /// Whether this is a link in a snapshot chain.
    pub fn has_parent(&self) -> bool {
        self.parent_cid != NO_PARENT_CID || self.parent_file_name_hint.is_some()
    }

    /// Names what makes this disk unreadable for a plain reader, if anything: encryption or other I/O filters.
    pub fn io_filter_marker(&self) -> Option<String> {
        self.entries
            .iter()
            .find(|(key, _)| {
                key.starts_with("ddb.iofilters")
                    || key.starts_with("ddb.sidecars")
                    || key.contains("encryption")
                    || key.contains("keyprovider")
            })
            .map(|(key, value)| format!("{key} = {value:?}"))
    }
}

/// Parses an extent line, or returns `None` if the line does not start with an access keyword.
fn parse_extent(line: &str, number: usize) -> Result<Option<ExtentLine>> {
    let (first, rest) = match line.split_once(char::is_whitespace) {
        Some(parts) => parts,
        None => return Ok(None),
    };
    let access = match first.to_ascii_uppercase().as_str() {
        "RW" => Access::ReadWrite,
        "RDONLY" => Access::ReadOnly,
        "NOACCESS" => Access::NoAccess,
        _ => return Ok(None),
    };
    let bad = |problem: &str| Error::corrupt(format!("descriptor line {number}: {problem}: {line:?}"));

    let rest = rest.trim_start();
    let (sectors, rest) = rest
        .split_once(char::is_whitespace)
        .ok_or_else(|| bad("extent has no type"))?;
    let sectors = sectors.parse::<u64>().map_err(|_| bad("extent size is not a number"))?;
    let rest = rest.trim_start();
    let (kind, rest) = match rest.split_once(char::is_whitespace) {
        Some((kind, rest)) => (kind, rest.trim()),
        None => (rest, ""),
    };
    let kind = ExtentType::parse(kind);

    // The file name is everything between the first and the last double quote.
    let (file_name, tail) = if rest.is_empty() {
        (String::new(), "")
    } else {
        let after_open = rest
            .strip_prefix('"')
            .ok_or_else(|| bad("extent file name is not quoted"))?;
        let close = after_open
            .rfind('"')
            .ok_or_else(|| bad("extent file name has no closing quote"))?;
        let (name, tail) = after_open.split_at(close);
        (name.to_owned(), tail.get(1..).unwrap_or_default().trim())
    };
    if file_name.is_empty() && kind != ExtentType::Zero {
        return Err(bad("extent has no file name"));
    }
    let offset = match tail {
        "" => None,
        text => Some(text.parse::<u64>().map_err(|_| bad("extent offset is not a number"))?),
    };
    Ok(Some(ExtentLine {
        access,
        sectors,
        kind,
        file_name,
        offset,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    const FLAT: &str = r#"# Disk DescriptorFile
version=1
encoding="UTF-8"
CID=fffffffe
parentCID=ffffffff
createType="vmfs"

# Extent description
RW 4192256 VMFS "web 01-flat.vmdk"

# The Disk Data Base
#DDB

ddb.adapterType = "lsilogic"
ddb.geometry.cylinders = "260"
ddb.thinProvisioned = "1"
ddb.uuid = "60 00 C2 9c 4b 8a 3b 6b-5c 0c 2d a1 1e 6f 7c 11"
"#;

    #[test]
    fn parses_a_flat_disk() {
        let d = Descriptor::parse(FLAT.as_bytes()).unwrap();
        assert_eq!(d.version, 1);
        assert_eq!(d.cid, 0xFFFF_FFFE);
        assert_eq!(d.parent_cid, NO_PARENT_CID);
        assert_eq!(d.create_type, "vmfs");
        assert!(!d.has_parent());
        assert!(d.thin_provisioned());
        assert_eq!(d.get("DDB.ADAPTERTYPE"), Some("lsilogic"));
        assert_eq!(
            d.extents,
            [ExtentLine {
                access: Access::ReadWrite,
                sectors: 4_192_256,
                kind: ExtentType::Vmfs,
                file_name: "web 01-flat.vmdk".into(),
                offset: None,
            }]
        );
        assert!(d.io_filter_marker().is_none());
    }

    #[test]
    fn parses_a_snapshot_link() {
        let text = "version=1\nCID=1234abcd\nparentCID=fffffffe\nisNativeSnapshot=\"no\"\ncreateType=\"seSparse\"\n\
                    parentFileNameHint=\"vm.vmdk\"\nRW 4192256 SESPARSE \"vm-000001-sesparse.vmdk\"\n\
                    ddb.longContentID = \"0123\"\n";
        let d = Descriptor::parse(text.as_bytes()).unwrap();
        assert!(d.has_parent());
        assert_eq!(d.parent_cid, 0xFFFF_FFFE);
        assert_eq!(d.parent_file_name_hint.as_deref(), Some("vm.vmdk"));
        assert_eq!(d.extents[0].kind, ExtentType::SeSparse);
    }

    #[test]
    fn keys_and_keywords_ignore_case_and_offsets_parse() {
        let text = "VERSION=3\ncid=00000001\nPARENTCID=FFFFFFFF\nCreateType=vmfsRaw\n\
                    rdonly 100 vmfsraw \"/vmfs/devices/disks/naa.1\" 63\nchangeTrackPath=\"x-ctk.vmdk\"\n";
        let d = Descriptor::parse(text.as_bytes()).unwrap();
        assert_eq!(d.version, 3);
        assert_eq!(d.extents[0].access, Access::ReadOnly);
        assert_eq!(d.extents[0].kind, ExtentType::VmfsRaw);
        assert_eq!(d.extents[0].offset, Some(63));
        assert_eq!(d.get("changetrackpath"), Some("x-ctk.vmdk"));
    }

    #[test]
    fn file_names_may_contain_quotes_spaces_and_non_ascii() {
        let text = "version=1\nCID=1\nparentCID=ffffffff\ncreateType=\"vmfs\"\nRW 8 VMFS \"dätä \"x\" 😀-flat.vmdk\"\n";
        let d = Descriptor::parse(text.as_bytes()).unwrap();
        assert_eq!(d.extents[0].file_name, "dätä \"x\" 😀-flat.vmdk");
    }

    #[test]
    fn detects_encryption_and_filters() {
        let text = format!("{FLAT}ddb.iofilters = \"spif:vmwarevmcrypt\"\n");
        let d = Descriptor::parse(text.as_bytes()).unwrap();
        assert!(d.io_filter_marker().unwrap().contains("vmwarevmcrypt"));
    }

    #[test]
    fn trailing_nul_padding_is_ignored() {
        let mut raw = FLAT.as_bytes().to_vec();
        raw.extend_from_slice(&[0u8; 100]);
        assert!(Descriptor::parse(&raw).is_ok());
    }

    #[test]
    fn malformed_descriptors_are_rejected() {
        let kind = |text: &str| Descriptor::parse(text.as_bytes()).unwrap_err().kind();
        assert_eq!(kind("version=1\nCID=1\ncreateType=vmfs\n"), ErrorKind::Corrupt); // no parentCID
        assert_eq!(
            kind("version=x\nCID=1\nparentCID=ffffffff\ncreateType=vmfs\n"),
            ErrorKind::Corrupt
        );
        assert_eq!(
            kind("version=1\nCID=zz\nparentCID=ffffffff\ncreateType=vmfs\n"),
            ErrorKind::Corrupt
        );
        assert_eq!(
            kind("version=1\nversion=1\nCID=1\nparentCID=ffffffff\ncreateType=vmfs\n"),
            ErrorKind::Corrupt
        );
        assert_eq!(kind("just some text\n"), ErrorKind::Corrupt);
        assert_eq!(kind("RW ten VMFS \"a\"\n"), ErrorKind::Corrupt);
        assert_eq!(kind("RW 10 VMFS a-flat.vmdk\n"), ErrorKind::Corrupt);
        assert_eq!(kind("RW 10 VMFS \"a-flat.vmdk\n"), ErrorKind::Corrupt);
        assert_eq!(kind("RW 10 VMFS\n"), ErrorKind::Corrupt);
        assert_eq!(kind("RW 10 VMFS \"a\" x\n"), ErrorKind::Corrupt);
        assert_eq!(
            Descriptor::parse(&[0xFF, 0xFE, 0x00, 0x41]).unwrap_err().kind(),
            ErrorKind::Unsupported
        );
    }
}
