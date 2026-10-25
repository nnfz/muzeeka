//! Read-only walker for an unencrypted Realm file (cluster layout, format 20–24).
//!
//! osu!lazer writes `client.realm` with this layout. The group root is still
//! slot 0 = table names, slot 1 = tables. Each table stores rows in a cluster
//! tree: inner nodes at slots [key ref, depth, size, children…], leaves at
//! slot 0 = key ref or tagged size, then one array per column.

const HEADER_SIZE: usize = 24;
const NODE_HEADER: usize = 8;
const NODE_SHIFT: u32 = 8;

#[derive(Clone, Copy, Debug)]
struct NodeHeader {
    is_inner: bool,
    has_refs: bool,
    context: bool,
    /// 0 = bits, 1 = bytes, 2 = raw byte blob.
    wtype: u8,
    /// Bit width when `wtype == 0`, byte width when `wtype == 1`.
    width: usize,
    size: usize,
}

impl NodeHeader {
    fn parse(buf: &[u8]) -> Result<Self, String> {
        if buf.len() < NODE_HEADER {
            return Err("realm node header is truncated".into());
        }
        let flags = buf[4];
        let size = ((buf[5] as usize) << 16) | ((buf[6] as usize) << 8) | (buf[7] as usize);
        let width_ndx = (flags & 0x07) as u32;
        let width = ((1usize << width_ndx) >> 1).min(64);
        Ok(Self {
            is_inner: flags & 0x80 != 0,
            has_refs: flags & 0x40 != 0,
            context: flags & 0x20 != 0,
            wtype: (flags & 0x18) >> 3,
            width,
            size,
        })
    }

    fn payload_len(self) -> Result<usize, String> {
        let len = match self.wtype {
            0 => self
                .size
                .checked_mul(self.width)
                .map(|bits| (bits + 7) / 8),
            1 => self.size.checked_mul(self.width),
            2 => Some(self.size),
            _ => return Err(format!("realm node has width scheme {}", self.wtype)),
        };
        len.ok_or_else(|| "realm node size overflow".into())
    }
}

#[derive(Clone, Copy)]
struct Node<'a> {
    header: NodeHeader,
    payload: &'a [u8],
}

impl<'a> Node<'a> {
    fn get_u64(self, index: usize) -> u64 {
        if index >= self.header.size || self.header.width == 0 {
            return 0;
        }
        match self.header.wtype {
            0 if self.header.width == 64 => read_le_u64(self.payload, index * 8),
            0 if self.header.width == 32 => read_le_u32(self.payload, index * 4) as u64,
            0 if self.header.width == 16 => read_le_u16(self.payload, index * 2) as u64,
            0 if self.header.width == 8 => self.payload.get(index).copied().unwrap_or(0) as u64,
            0 => read_bits(self.payload, self.header.width, index),
            1 => read_le_bytes(self.payload, index * self.header.width, self.header.width),
            2 => self.payload.get(index).copied().unwrap_or(0) as u64,
            _ => 0,
        }
    }

    fn get_i64(self, index: usize) -> i64 {
        let bits = self.header.wtype == 0;
        let width_bits = if bits {
            self.header.width
        } else if self.header.wtype == 1 {
            self.header.width.saturating_mul(8)
        } else {
            8
        };
        sign_extend(self.get_u64(index), width_bits)
    }

    /// Even non-zero values are refs. Odd values are tagged integers. Zero is empty.
    fn slot(self, index: usize) -> Slot {
        let value = self.get_u64(index);
        if value == 0 {
            Slot::Empty
        } else if value & 1 == 0 {
            Slot::Ref(value as usize)
        } else {
            Slot::Tagged
        }
    }

    fn ref_at(self, index: usize) -> Option<usize> {
        match self.slot(index) {
            Slot::Ref(value) if value % 8 == 0 => Some(value),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Slot {
    Empty,
    Ref(usize),
    Tagged,
}

pub struct Realm<'a> {
    data: &'a [u8],
    top: usize,
}

impl<'a> Realm<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, String> {
        if data.len() < HEADER_SIZE {
            return Err("realm file is too small".into());
        }
        if &data[16..20] != b"T-DB" {
            return Err("not a realm file".into());
        }
        let flags = data[23];
        if flags & 0x80 != 0 {
            return Err("encrypted realm files are not supported".into());
        }
        let which = (flags & 1) as usize;
        let top = u64::from_le_bytes(data[which * 8..which * 8 + 8].try_into().unwrap()) as usize;
        if top == 0 || top % 8 != 0 || top + NODE_HEADER > data.len() {
            return Err(format!("realm top ref 0x{top:X} is outside the file"));
        }
        Ok(Self { data, top })
    }

    pub fn table(&self, name: &str) -> Result<Table<'a>, String> {
        let tables = self.tables()?;
        let (_, table_ref) = tables
            .iter()
            .find(|(found, _)| found == name)
            .ok_or_else(|| {
                format!(
                    "realm has no table {name} (have: {})",
                    tables
                        .iter()
                        .map(|(n, _)| n.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
        self.open_table(name, *table_ref)
    }

    fn tables(&self) -> Result<Vec<(String, usize)>, String> {
        let top = self.node(self.top)?;
        let names_ref = top
            .ref_at(0)
            .ok_or("realm group is missing table names")?;
        let tables_ref = top.ref_at(1).ok_or("realm group is missing tables")?;
        let names = self.strings(names_ref)?;
        let tables = self.node(tables_ref)?;
        if names.len() != tables.header.size {
            return Err(format!(
                "realm table name count {} does not match table refs {}",
                names.len(),
                tables.header.size
            ));
        }
        let mut out = Vec::with_capacity(names.len());
        for (index, name) in names.into_iter().enumerate() {
            let Some(name) = name else { continue };
            let Some(table_ref) = tables.ref_at(index) else {
                continue;
            };
            out.push((name, table_ref));
        }
        Ok(out)
    }

    fn open_table(&self, name: &str, table_ref: usize) -> Result<Table<'a>, String> {
        let top = self.node(table_ref)?;
        let spec_ref = top
            .ref_at(0)
            .ok_or_else(|| format!("{name} is missing a spec"))?;
        let columns = self.read_spec(spec_ref)?;
        let mut leaves = Vec::new();
        if let Some(cluster) = top.ref_at(2) {
            self.walk_cluster(cluster, 0, 0, &mut leaves)?;
        }
        Ok(Table {
            data: self.data,
            name: name.to_string(),
            columns,
            leaves,
        })
    }

    fn read_spec(&self, spec_ref: usize) -> Result<Vec<Column>, String> {
        let spec = self.node(spec_ref)?;
        let names_ref = spec.ref_at(1).ok_or("realm spec is missing column names")?;
        let keys_ref = spec.ref_at(5).ok_or("realm spec is missing column keys")?;
        let names = self.strings(names_ref)?;
        // Public columns share an index with `names`. Trailing keys are
        // unnamed backlinks and are kept so their cluster slots stay aligned.
        let key_vals = self.i64s(keys_ref)?;
        if key_vals.len() < names.len() {
            return Err(format!(
                "realm spec has {} names and {} column keys",
                names.len(),
                key_vals.len()
            ));
        }
        let enum_refs = match spec.ref_at(4) {
            Some(enum_ref) => self.ref_list(enum_ref)?,
            None => Vec::new(),
        };
        let mut columns = Vec::with_capacity(key_vals.len());
        for (position, bits) in key_vals.iter().copied().enumerate() {
            let bits = bits as u64;
            let index = (bits & 0xFFFF) as usize;
            let by_key = enum_refs.get(index).copied().flatten();
            let by_pos = enum_refs.get(position).copied().flatten();
            let name = names
                .get(position)
                .cloned()
                .flatten()
                .unwrap_or_default();
            columns.push(Column {
                name,
                index,
                attrs: ((bits >> 22) & 0xFF) as u8,
                enum_ref: by_key.or(by_pos),
            });
        }
        Ok(columns)
    }

    fn walk_cluster(
        &self,
        node_ref: usize,
        parent_offset: i64,
        depth: u32,
        out: &mut Vec<Leaf>,
    ) -> Result<(), String> {
        if depth > 32 {
            return Err("realm cluster is too deep".into());
        }
        let node = self.node(node_ref)?;
        if !node.header.is_inner {
            out.push(self.leaf(node, parent_offset)?);
            return Ok(());
        }
        if node.header.size < 3 {
            return Err("realm cluster node is truncated".into());
        }
        let sub_depth = (node.get_u64(1) >> 1) as u32;
        let shift = sub_depth.saturating_mul(NODE_SHIFT).min(62);
        let key_slot = node.get_u64(0);
        // Child offsets are unsigned. A 16-bit value above 32767 is still positive.
        let offsets = if key_slot != 0 && key_slot & 1 == 0 {
            Some(self.u64s(key_slot as usize)?)
        } else {
            None
        };
        let child_count = node.header.size.saturating_sub(3);
        for index in 0..child_count {
            let Some(child) = node.ref_at(3 + index) else {
                continue;
            };
            let child_offset = if let Some(offsets) = &offsets {
                parent_offset.wrapping_add(offsets.get(index).copied().unwrap_or(0) as i64)
            } else {
                parent_offset.wrapping_add((index as i64) << shift)
            };
            self.walk_cluster(child, child_offset, depth + 1, out)?;
        }
        Ok(())
    }

    fn leaf(&self, node: Node<'a>, parent_offset: i64) -> Result<Leaf, String> {
        if node.header.size == 0 {
            return Ok(Leaf {
                keys: Vec::new(),
                columns: Vec::new(),
            });
        }
        let slot0 = node.get_u64(0);
        let keys = if slot0 & 1 == 1 {
            let count = (slot0 >> 1) as usize;
            (0..count)
                .map(|index| parent_offset.wrapping_add(index as i64))
                .collect()
        } else if slot0 == 0 {
            Vec::new()
        } else {
            // Cluster key arrays are unsigned. Sign-extending a 16-bit key
            // turns 32768+ into a negative id and breaks links.
            self.u64s(slot0 as usize)?
                .into_iter()
                .map(|key| parent_offset.wrapping_add(key as i64))
                .collect()
        };
        let mut columns = Vec::new();
        for index in 1..node.header.size {
            columns.push(node.ref_at(index));
        }
        Ok(Leaf { keys, columns })
    }

    fn node(&self, offset: usize) -> Result<Node<'a>, String> {
        if offset % 8 != 0 || offset + NODE_HEADER > self.data.len() {
            return Err(format!("realm ref 0x{offset:X} is outside the file"));
        }
        let header = NodeHeader::parse(&self.data[offset..offset + NODE_HEADER])?;
        let len = header.payload_len()?;
        let start = offset + NODE_HEADER;
        let end = start.checked_add(len).ok_or("realm node size overflow")?;
        if end > self.data.len() {
            return Err(format!(
                "realm node at 0x{offset:X} claims {len} payload bytes past the end of the file"
            ));
        }
        Ok(Node {
            header,
            payload: &self.data[start..end],
        })
    }

    fn i64s(&self, offset: usize) -> Result<Vec<i64>, String> {
        let node = self.node(offset)?;
        Ok((0..node.header.size).map(|index| node.get_i64(index)).collect())
    }

    fn u64s(&self, offset: usize) -> Result<Vec<u64>, String> {
        let node = self.node(offset)?;
        Ok((0..node.header.size).map(|index| node.get_u64(index)).collect())
    }

    fn ref_list(&self, offset: usize) -> Result<Vec<Option<usize>>, String> {
        let node = self.node(offset)?;
        Ok((0..node.header.size).map(|index| node.ref_at(index)).collect())
    }

    fn strings(&self, offset: usize) -> Result<Vec<Option<String>>, String> {
        let node = self.node(offset)?;
        if node.header.is_inner {
            let mut out = Vec::new();
            self.flatten_strings(node, &mut out)?;
            return Ok(out);
        }
        if !node.header.has_refs {
            return Ok(short_strings(node));
        }
        if !node.header.context {
            self.medium_strings(node)
        } else {
            self.big_strings(node)
        }
    }

    fn flatten_strings(&self, node: Node<'a>, out: &mut Vec<Option<String>>) -> Result<(), String> {
        if !node.header.is_inner {
            out.extend(self.strings_leaf(node)?);
            return Ok(());
        }
        // B+ tree inner: slot 0 is offsets or a tagged fanout, the last slot is the size.
        if node.header.size < 2 {
            return Ok(());
        }
        for index in 1..node.header.size.saturating_sub(1) {
            if let Some(child) = node.ref_at(index) {
                let child = self.node(child)?;
                self.flatten_strings(child, out)?;
            }
        }
        Ok(())
    }

    fn strings_leaf(&self, node: Node<'a>) -> Result<Vec<Option<String>>, String> {
        if !node.header.has_refs {
            return Ok(short_strings(node));
        }
        if !node.header.context {
            self.medium_strings(node)
        } else {
            self.big_strings(node)
        }
    }

    fn medium_strings(&self, node: Node<'a>) -> Result<Vec<Option<String>>, String> {
        if node.header.size < 2 {
            return Err("realm string array is truncated".into());
        }
        let offsets_ref = node
            .ref_at(0)
            .ok_or("realm string array is missing offsets")?;
        let blob_ref = node
            .ref_at(1)
            .ok_or("realm string array is missing its blob")?;
        let offsets = self.u64s(offsets_ref)?;
        let blob = self.blob_bytes(blob_ref)?;
        let nulls = match node.ref_at(2) {
            Some(nulls_ref) => Some(self.u64s(nulls_ref)?),
            None => None,
        };
        let mut out = Vec::with_capacity(offsets.len());
        for index in 0..offsets.len() {
            if nulls.as_ref().and_then(|flags| flags.get(index)).copied().unwrap_or(0) != 0 {
                out.push(None);
                continue;
            }
            let begin = if index == 0 {
                0
            } else {
                offsets[index - 1] as usize
            };
            let end = offsets[index] as usize;
            if begin > end || end > blob.len() {
                return Err(format!(
                    "realm string offsets {begin}..{end} do not fit a {} byte blob",
                    blob.len()
                ));
            }
            out.push(Some(blob_string(&blob[begin..end])));
        }
        Ok(out)
    }

    fn big_strings(&self, node: Node<'a>) -> Result<Vec<Option<String>>, String> {
        let mut out = Vec::with_capacity(node.header.size);
        for index in 0..node.header.size {
            let Some(blob_ref) = node.ref_at(index) else {
                out.push(None);
                continue;
            };
            let blob_node = self.node(blob_ref)?;
            if blob_node.header.context {
                out.push(None);
                continue;
            }
            out.push(Some(blob_string(blob_node.payload)));
        }
        Ok(out)
    }

    fn blob_bytes(&self, offset: usize) -> Result<&'a [u8], String> {
        Ok(self.node(offset)?.payload)
    }
}

fn short_strings(node: Node<'_>) -> Vec<Option<String>> {
    let width = if node.header.wtype == 1 {
        node.header.width
    } else {
        0
    };
    let mut out = Vec::with_capacity(node.header.size);
    if width == 0 {
        out.resize(node.header.size, None);
        return out;
    }
    for index in 0..node.header.size {
        let start = index * width;
        let Some(elem) = node.payload.get(start..start + width) else {
            out.push(None);
            continue;
        };
        let zeroes = elem[width - 1] as usize;
        if zeroes >= width {
            out.push(None);
            continue;
        }
        let len = width - 1 - zeroes;
        out.push(Some(String::from_utf8_lossy(&elem[..len]).into_owned()));
    }
    out
}

/// Realm string blobs include a trailing NUL in the stored length.
fn blob_string(bytes: &[u8]) -> String {
    let end = bytes.len().saturating_sub(1);
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn read_bits(payload: &[u8], width: usize, index: usize) -> u64 {
    if width == 0 || width > 64 {
        return 0;
    }
    let start = index * width;
    let mut value = 0u64;
    for bit in 0..width {
        let pos = start + bit;
        let Some(byte) = payload.get(pos / 8) else {
            break;
        };
        let flag = (byte >> (pos % 8)) & 1;
        value |= (flag as u64) << bit;
    }
    value
}

fn read_le_bytes(payload: &[u8], start: usize, width: usize) -> u64 {
    let width = width.min(8);
    let mut buf = [0u8; 8];
    let Some(src) = payload.get(start..start + width) else {
        return 0;
    };
    buf[..width].copy_from_slice(src);
    u64::from_le_bytes(buf)
}

fn read_le_u16(payload: &[u8], start: usize) -> u16 {
    payload
        .get(start..start + 2)
        .and_then(|b| b.try_into().ok())
        .map(u16::from_le_bytes)
        .unwrap_or(0)
}

fn read_le_u32(payload: &[u8], start: usize) -> u32 {
    payload
        .get(start..start + 4)
        .and_then(|b| b.try_into().ok())
        .map(u32::from_le_bytes)
        .unwrap_or(0)
}

fn read_le_u64(payload: &[u8], start: usize) -> u64 {
    payload
        .get(start..start + 8)
        .and_then(|b| b.try_into().ok())
        .map(u64::from_le_bytes)
        .unwrap_or(0)
}

fn sign_extend(value: u64, bits: usize) -> i64 {
    if bits == 0 || bits >= 64 {
        return value as i64;
    }
    let shift = 64 - bits;
    ((value << shift) as i64) >> shift
}

#[derive(Clone, Debug)]
pub struct Column {
    pub name: String,
    pub index: usize,
    pub attrs: u8,
    pub enum_ref: Option<usize>,
}

impl Column {
    pub fn is_list(&self) -> bool {
        self.attrs & 32 != 0
    }
}

struct Leaf {
    keys: Vec<i64>,
    /// Column arrays, indexed by [`Column::index`]. Slot 0 of the cluster is the key array.
    columns: Vec<Option<usize>>,
}

pub struct Table<'a> {
    data: &'a [u8],
    name: String,
    columns: Vec<Column>,
    leaves: Vec<Leaf>,
}

impl<'a> Table<'a> {
    fn realm(&self) -> Realm<'a> {
        Realm {
            data: self.data,
            top: 0,
        }
    }

    pub fn column(&self, names: &[&str]) -> Result<&Column, String> {
        for name in names {
            if let Some(column) = self
                .columns
                .iter()
                .find(|column| column.name.eq_ignore_ascii_case(name))
            {
                return Ok(column);
            }
        }
        Err(format!(
            "{} has no column {} (have: {})",
            self.name,
            names.join("/"),
            self.columns
                .iter()
                .map(|column| column.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }

    pub fn strings(&self, names: &[&str]) -> Result<Vec<(i64, Option<String>)>, String> {
        let column = self.column(names)?;
        let dict = match column.enum_ref {
            Some(enum_ref) => Some(self.realm().strings(enum_ref)?),
            None => None,
        };
        let mut out = Vec::new();
        for leaf in &self.leaves {
            let values = if let Some(dict) = &dict {
                self.indexed_strings(leaf, column, dict)?
            } else {
                self.leaf_strings(leaf, column)?
            };
            if values.len() != leaf.keys.len() {
                return Err(format!(
                    "{}.{} has {} values for {} rows",
                    self.name,
                    column.name,
                    values.len(),
                    leaf.keys.len()
                ));
            }
            out.extend(leaf.keys.iter().copied().zip(values));
        }
        Ok(out)
    }

    /// Object keys stored in a link column. Realm stores `key + 1` so null is 0.
    pub fn links(&self, names: &[&str]) -> Result<Vec<(i64, Option<i64>)>, String> {
        let column = self.column(names)?;
        let mut out = Vec::new();
        for leaf in &self.leaves {
            let values = self.leaf_i64(leaf, column)?;
            if values.len() != leaf.keys.len() {
                return Err(format!(
                    "{}.{} has {} links for {} rows",
                    self.name,
                    column.name,
                    values.len(),
                    leaf.keys.len()
                ));
            }
            out.extend(leaf.keys.iter().copied().zip(values.into_iter().map(link_key)));
        }
        Ok(out)
    }

    /// Link lists. Values inside the list tree are raw object keys.
    pub fn link_lists(&self, names: &[&str]) -> Result<Vec<(i64, Vec<i64>)>, String> {
        let column = self.column(names)?;
        let mut out = Vec::new();
        for leaf in &self.leaves {
            let lists = self.leaf_lists(leaf, column)?;
            if lists.len() != leaf.keys.len() {
                return Err(format!(
                    "{}.{} has {} lists for {} rows",
                    self.name,
                    column.name,
                    lists.len(),
                    leaf.keys.len()
                ));
            }
            out.extend(leaf.keys.iter().copied().zip(lists));
        }
        Ok(out)
    }

    fn leaf_strings(&self, leaf: &Leaf, column: &Column) -> Result<Vec<Option<String>>, String> {
        let Some(array) = leaf.columns.get(column.index).copied().flatten() else {
            return Ok(vec![None; leaf.keys.len()]);
        };
        let mut values = self.realm().strings(array)?;
        if values.len() == leaf.keys.len() + 1 {
            values.remove(0);
        }
        Ok(values)
    }

    fn indexed_strings(
        &self,
        leaf: &Leaf,
        column: &Column,
        dict: &[Option<String>],
    ) -> Result<Vec<Option<String>>, String> {
        let indexes = self.leaf_i64(leaf, column)?;
        Ok(indexes
            .into_iter()
            .map(|index| {
                if index < 0 {
                    None
                } else {
                    dict.get(index as usize).cloned().flatten()
                }
            })
            .collect())
    }

    fn leaf_i64(&self, leaf: &Leaf, column: &Column) -> Result<Vec<i64>, String> {
        let Some(array) = leaf.columns.get(column.index).copied().flatten() else {
            return Ok(vec![0; leaf.keys.len()]);
        };
        let mut values = self.realm().i64s(array)?;
        if values.len() == leaf.keys.len() + 1 {
            values.remove(0);
        }
        Ok(values)
    }

    fn leaf_lists(&self, leaf: &Leaf, column: &Column) -> Result<Vec<Vec<i64>>, String> {
        let Some(array) = leaf.columns.get(column.index).copied().flatten() else {
            return Ok(vec![Vec::new(); leaf.keys.len()]);
        };
        let node = self.realm().node(array)?;
        let mut lists = Vec::with_capacity(node.header.size);
        for index in 0..node.header.size {
            let Some(root) = node.ref_at(index) else {
                lists.push(Vec::new());
                continue;
            };
            let mut keys = Vec::new();
            self.collect_list(root, &mut keys, 0)?;
            lists.push(keys);
        }
        if lists.len() == leaf.keys.len() + 1 {
            lists.remove(0);
        }
        Ok(lists)
    }

    fn collect_list(&self, node_ref: usize, out: &mut Vec<i64>, depth: u32) -> Result<(), String> {
        if depth > 32 {
            return Err("realm list is too deep".into());
        }
        let node = self.realm().node(node_ref)?;
        if node.header.is_inner {
            if node.header.size < 2 {
                return Ok(());
            }
            for index in 1..node.header.size.saturating_sub(1) {
                if let Some(child) = node.ref_at(index) {
                    self.collect_list(child, out, depth + 1)?;
                }
            }
            return Ok(());
        }
        for index in 0..node.header.size {
            let key = node.get_i64(index);
            if key >= 0 {
                out.push(key);
            }
        }
        Ok(())
    }
}

fn link_key(stored: i64) -> Option<i64> {
    let key = stored.wrapping_sub(1);
    if key < 0 {
        None
    } else {
        Some(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osu_top_node_header_is_eleven_refs() {
        let header = [0x41, 0x41, 0x41, 0x41, 0x46, 0x00, 0x00, 0x0B];
        let parsed = NodeHeader::parse(&header).unwrap();
        assert!(parsed.has_refs);
        assert!(!parsed.is_inner);
        assert_eq!(parsed.wtype, 0);
        assert_eq!(parsed.width, 32);
        assert_eq!(parsed.size, 11);
    }

    #[test]
    fn short_string_width_matches_table_names() {
        let header = [0x41, 0x41, 0x41, 0x41, 0x0F, 0x00, 0x00, 0x01];
        let parsed = NodeHeader::parse(&header).unwrap();
        assert!(!parsed.has_refs);
        assert_eq!(parsed.wtype, 1);
        assert_eq!(parsed.width, 64);
        let mut payload = vec![0u8; 64];
        payload[..8].copy_from_slice(b"metadata");
        payload[63] = 55;
        let node = Node {
            header: parsed,
            payload: &payload,
        };
        let names = short_strings(node);
        assert_eq!(names, vec![Some("metadata".to_string())]);
    }

    #[test]
    fn sign_extend_keeps_negative_object_keys() {
        assert_eq!(sign_extend(0xFFFF_FFFF, 32), -1);
        assert_eq!(sign_extend(5, 32), 5);
    }
}
