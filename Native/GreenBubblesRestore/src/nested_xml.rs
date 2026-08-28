use std::borrow::Cow;

use roxmltree::{Document, Node, NodeType, ParsingOptions};
use serde::Serialize;
use serde_json::Value;

use crate::{CanonicalMessage, SemanticDecodeState, TypedPayload};

const NORMALIZED_XML_FORMAT_VERSION: u32 = 1;
const MAX_XML_BYTES: usize = 8 * 1024 * 1024;
const MAX_XML_NODES: u32 = 100_000;
const MAX_EMBEDDED_DEPTH: usize = 4;

#[derive(Debug, Clone, Copy)]
enum NestedMessageKind {
    MergedMessages,
    ChannelMedia,
}

impl NestedMessageKind {
    fn variant_name(self) -> &'static str {
        match self {
            Self::MergedMessages => "MergedMessages",
            Self::ChannelMedia => "ChannelVideo",
        }
    }

    fn gap_label(self) -> &'static str {
        match self {
            Self::MergedMessages => "merged-message nested XML",
            Self::ChannelMedia => "channel-media nested XML",
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NormalizedXmlDocument {
    format_version: u32,
    node_count: u64,
    embedded_document_count: u64,
    nodes: Vec<NormalizedXmlNode>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum NormalizedXmlNode {
    Element {
        name: String,
        namespace_uri: Option<String>,
        namespaces: Vec<NormalizedXmlNamespace>,
        attributes: Vec<NormalizedXmlAttribute>,
        children: Vec<NormalizedXmlNode>,
    },
    Text {
        value: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        embedded_document: Option<Box<NormalizedXmlDocument>>,
    },
    Comment {
        value: String,
    },
    ProcessingInstruction {
        target: String,
        value: Option<String>,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NormalizedXmlNamespace {
    prefix: Option<String>,
    uri: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NormalizedXmlAttribute {
    name: String,
    namespace_uri: Option<String>,
    value: String,
}

#[derive(Default)]
struct NormalizationCounts {
    node_count: u64,
    embedded_document_count: u64,
}

pub(crate) fn serialize_message_content(
    content: &wx_db::MessageContent,
) -> Result<(Value, Option<String>), String> {
    let mut value = serde_json::to_value(content).map_err(|error| error.to_string())?;
    let kind = match content {
        wx_db::MessageContent::MergedMessages { raw_xml, .. } => {
            enrich_nested_value(&mut value, NestedMessageKind::MergedMessages, raw_xml)?
        }
        wx_db::MessageContent::ChannelVideo { raw_xml, .. } => {
            enrich_nested_value(&mut value, NestedMessageKind::ChannelMedia, raw_xml)?
        }
        wx_db::MessageContent::AppGeneric {
            sub_type, raw_xml, ..
        } => {
            // Generic app subtypes are still semantically useful when their
            // outer `<msg><appmsg>` document is valid.  The subtype-specific
            // fields remain losslessly available in `raw_xml`; validate the
            // structural envelope rather than downgrading every unknown
            // subtype to a gap.  Malformed payloads retain an explicit gap.
            if let Err(error) = validate_app_xml(raw_xml) {
                return Ok((
                    value,
                    Some(format!(
                        "app message subtype {sub_type} has only generic XML decoding: {error}"
                    )),
                ));
            }
            return Ok((value, None));
        }
        _ => return Ok((value, None)),
    };
    match kind {
        Ok(()) => Ok((value, None)),
        Err(reason) => Ok((value, Some(reason))),
    }
}

/// Validate the outer XML envelope of a generic app message without trying to
/// interpret subtype-specific fields.  The caller retains the original XML,
/// so this check is deliberately structural and privacy-neutral.
pub(crate) fn validate_app_xml(raw_xml: &str) -> Result<(), String> {
    let candidate = xml_document_candidate(raw_xml)
        .ok_or_else(|| "XML does not contain a <msg> document".to_string())?;
    let repaired = repair_xml_for_parsing(candidate);
    let candidate = repaired.as_ref();
    let parsed = Document::parse_with_options(
        candidate,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_XML_NODES,
            entity_resolver: None,
        },
    )
    .map_err(|error| format!("XML is malformed: {error}"))?;
    if !parsed.descendants().any(|node| node.has_tag_name("appmsg")) {
        return Err("the appmsg root is absent".to_string());
    }
    Ok(())
}

/// Produce the bounded, deterministic XML projection used for legacy message
/// types that are not represented by wx-db's typed enum (for example contact
/// cards and VoIP call notices).  The raw XML is kept separately by the
/// caller; this value contains only parsed structure and therefore gives
/// downstream consumers a stable shape to inspect.
pub(crate) fn normalize_xml_projection(raw_xml: &str) -> Result<Value, String> {
    let candidate = xml_document_candidate(raw_xml)
        .ok_or_else(|| "XML does not contain a supported document root".to_string())?;
    let document = parse_document(candidate, 0)?;
    serde_json::to_value(document).map_err(|error| error.to_string())
}

/// Normalize the observed legacy type-50 representation where WeChat stores
/// a VoIP invitation and local-call metadata as adjacent XML documents rather
/// than under a single `<msg>` root. The source string is never rewritten in
/// the archive; the synthetic root exists only in the deterministic parsed
/// projection. Keep this deliberately narrow so arbitrary malformed XML is
/// not promoted to a complete semantic decode.
pub(crate) fn normalize_voip_xml_projection(raw_xml: &str) -> Result<Value, String> {
    if let Ok(value) = normalize_xml_projection(raw_xml) {
        return Ok(value);
    }

    let trimmed = raw_xml.trim();
    if trimmed.is_empty() {
        return Err("VoIP XML is empty".to_string());
    }
    if trimmed.len() > MAX_XML_BYTES {
        return Err(format!(
            "VoIP XML exceeds the {}-byte normalization limit",
            MAX_XML_BYTES
        ));
    }
    let wrapped = format!("<greenbubbles-voip-fragments>{trimmed}</greenbubbles-voip-fragments>");
    let repaired = repair_xml_for_parsing(&wrapped);
    let parsed = Document::parse_with_options(
        repaired.as_ref(),
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_XML_NODES,
            entity_resolver: None,
        },
    )
    .map_err(|error| format!("VoIP fragment XML is malformed: {error}"))?;
    let root = parsed.root_element();
    let mut invitation_count = 0_u64;
    let mut extension_info_count = 0_u64;
    let mut local_info_count = 0_u64;
    for child in root.children().filter(|node| node.is_element()) {
        match child.tag_name().name().to_ascii_lowercase().as_str() {
            "voipinvitemsg" => invitation_count = invitation_count.saturating_add(1),
            "voipextinfo" => extension_info_count = extension_info_count.saturating_add(1),
            "voiplocalinfo" => local_info_count = local_info_count.saturating_add(1),
            other => {
                return Err(format!(
                    "VoIP fragment XML contains unsupported top-level element {other}"
                ))
            }
        }
    }
    if invitation_count != 1 || extension_info_count > 1 || local_info_count != 1 {
        return Err(
            "VoIP fragment XML must contain one voipinvitemsg, at most one voipextinfo, and one voiplocalinfo root".to_string(),
        );
    }

    let document = parse_exact_document(repaired.as_ref(), 0)?;
    serde_json::to_value(document).map_err(|error| error.to_string())
}

pub(crate) fn xml_has_element_or_attribute(raw_xml: &str, requested_name: &str) -> bool {
    let Some(candidate) = xml_document_candidate(raw_xml) else {
        return false;
    };
    let repaired = repair_xml_for_parsing(candidate);
    let candidate = repaired.as_ref();
    let Ok(parsed) = Document::parse_with_options(
        candidate,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_XML_NODES,
            entity_resolver: None,
        },
    ) else {
        return has_tag_shaped_name(raw_xml, requested_name);
    };
    if parsed.descendants().any(|node| {
        node.is_element()
            && (node.tag_name().name().eq_ignore_ascii_case(requested_name)
                || node
                    .attributes()
                    .any(|attribute| attribute.name().eq_ignore_ascii_case(requested_name)))
    }) {
        return true;
    }
    // Some VoIP rows wrap the `<msg>` document in a non-XML transport
    // envelope (for example `<voipmsg>...<msg>...</msg>...</voipmsg>`).
    // The message projection intentionally uses the inner `<msg>` slice, but
    // its envelope still carries the discriminator.  Check tag-shaped
    // occurrences in the preserved source without treating arbitrary text as
    // a semantic marker.
    has_tag_shaped_name(raw_xml, requested_name)
}

fn has_tag_shaped_name(xml: &str, requested_name: &str) -> bool {
    let lower = xml.to_ascii_lowercase();
    let needle = format!("<{}", requested_name.to_ascii_lowercase());
    let mut search = 0;
    while let Some(relative) = lower[search..].find(&needle) {
        let start = search + relative;
        let next = lower.as_bytes().get(start + needle.len()).copied();
        if next
            .is_some_and(|value| matches!(value, b'>' | b'/' | b' ' | b'\t' | b'\r' | b'\n' | b'='))
            && !inside_cdata_or_comment(&lower, start)
        {
            return true;
        }
        search = start.saturating_add(needle.len());
    }
    false
}

fn xml_document_candidate(raw_xml: &str) -> Option<&str> {
    let trimmed = raw_xml.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    let msg_start = lower
        .find("<msg")
        .filter(|start| is_msg_tag_start(&lower, *start));
    let starts_with_record_document =
        lower.starts_with("<recordinfo") || lower.starts_with("<recordxml");
    if let Some(start) = msg_start.filter(|start| *start > 0 && !starts_with_record_document) {
        let tail = &lower[start..];
        if let Some(close) = tail.rfind("</msg>") {
            let end = start.checked_add(close)?.checked_add("</msg>".len())?;
            return (end > start).then(|| &trimmed[start..end]);
        }
        if let Some(open_end) = find_tag_end(&trimmed[start..]) {
            let opening = &trimmed[start..=start + open_end];
            if opening.trim_end().ends_with("/>") {
                return Some(opening);
            }
        }
    }
    if let Some(start) = msg_start {
        let tail = &lower[start..];
        if let Some(close) = tail.rfind("</msg>") {
            let end = start.checked_add(close)?.checked_add("</msg>".len())?;
            return (end > start).then(|| &trimmed[start..end]);
        }
        // Contact-card rows commonly use a self-closing `<msg .../>` root.
        if let Some(open_end) = find_tag_end(&trimmed[start..]) {
            let opening = &trimmed[start..=start + open_end];
            if opening.trim_end().ends_with("/>") {
                return Some(opening);
            }
        }
    }
    Some(trimmed)
}

fn is_msg_tag_start(value: &str, start: usize) -> bool {
    let Some(next) = value.as_bytes().get(start.saturating_add(4)) else {
        return false;
    };
    matches!(next, b'>' | b'/' | b' ' | b'\t' | b'\r' | b'\n')
}

pub(crate) fn validate_canonical_message(message: &CanonicalMessage) -> Result<(), String> {
    let kind = match (message.logical_type, message.sub_type) {
        (Some(49), Some(19)) => NestedMessageKind::MergedMessages,
        (Some(49), Some(51 | 63)) => NestedMessageKind::ChannelMedia,
        _ => return Ok(()),
    };
    let TypedPayload::Decoded(value) = &message.typed_payload else {
        return Ok(());
    };
    let variant = value
        .get(kind.variant_name())
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{} typed payload has the wrong shape", kind.gap_label()))?;
    let raw_xml = variant
        .get("raw_xml")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{} lacks its source XML", kind.gap_label()))?;
    let observed = variant.get("normalized_xml");
    match normalize_for_kind(raw_xml, kind) {
        Ok(expected) => {
            let expected = serde_json::to_value(expected).map_err(|error| error.to_string())?;
            match observed {
                Some(observed) => {
                    if observed != &expected {
                        return Err(format!(
                            "{} projection differs from its source XML",
                            kind.gap_label()
                        ));
                    }
                    if message.semantic_decode_state != SemanticDecodeState::Complete
                        || message.semantic_gap_reason.is_some()
                    {
                        return Err(format!(
                            "{} is normalized but its semantic verdict is weaker",
                            kind.gap_label()
                        ));
                    }
                }
                None => {
                    if message.semantic_decode_state == SemanticDecodeState::Complete
                        || message.semantic_gap_reason.is_none()
                    {
                        return Err(format!(
                            "{} lacks its reproducible projection without recording a legacy semantic gap",
                            kind.gap_label()
                        ));
                    }
                }
            }
        }
        Err(_) => {
            if observed.is_some() {
                return Err(format!(
                    "{} contains an unverifiable normalized projection",
                    kind.gap_label()
                ));
            }
            if message.semantic_decode_state == SemanticDecodeState::Complete
                || message.semantic_gap_reason.is_none()
            {
                return Err(format!(
                    "{} normalization failure is not recorded as a semantic gap",
                    kind.gap_label()
                ));
            }
        }
    }
    Ok(())
}

fn enrich_nested_value(
    value: &mut Value,
    kind: NestedMessageKind,
    raw_xml: &str,
) -> Result<Result<(), String>, String> {
    let variant = value
        .get_mut(kind.variant_name())
        .and_then(Value::as_object_mut)
        .ok_or_else(|| {
            format!(
                "{} typed serialization has the wrong shape",
                kind.gap_label()
            )
        })?;
    match normalize_for_kind(raw_xml, kind) {
        Ok(document) => {
            variant.insert(
                "normalized_xml".to_string(),
                serde_json::to_value(document).map_err(|error| error.to_string())?,
            );
            Ok(Ok(()))
        }
        Err(error) => Ok(Err(format!(
            "{} could not be normalized: {error}",
            kind.gap_label()
        ))),
    }
}

fn normalize_for_kind(
    raw_xml: &str,
    kind: NestedMessageKind,
) -> Result<NormalizedXmlDocument, String> {
    let candidate = xml_document_candidate(raw_xml).unwrap_or(raw_xml);
    let repaired = repair_xml_for_parsing(candidate);
    let parse_xml = repaired.as_ref();
    let document = parse_document(parse_xml, 0)?;
    let parsed = Document::parse_with_options(
        parse_xml,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_XML_NODES,
            entity_resolver: None,
        },
    )
    .map_err(|error| format!("XML is malformed: {error}"))?;
    if !parsed.descendants().any(|node| node.has_tag_name("appmsg")) {
        return Err("the appmsg root is absent".to_string());
    }
    match kind {
        NestedMessageKind::MergedMessages => {
            if !parsed
                .descendants()
                .any(|node| node.has_tag_name("recorditem"))
                || document.embedded_document_count == 0
            {
                return Err("the embedded recorditem graph is absent".to_string());
            }
        }
        NestedMessageKind::ChannelMedia => {
            if !parsed.descendants().any(|node| {
                node.is_element()
                    && node
                        .tag_name()
                        .name()
                        .to_ascii_lowercase()
                        .starts_with("finder")
            }) {
                return Err("the Finder media graph is absent".to_string());
            }
        }
    }
    Ok(document)
}

fn parse_document(xml: &str, embedded_depth: usize) -> Result<NormalizedXmlDocument, String> {
    let candidate = xml_document_candidate(xml).unwrap_or(xml);
    let repaired = repair_xml_for_parsing(candidate);
    parse_exact_document(repaired.as_ref(), embedded_depth)
}

fn parse_exact_document(xml: &str, embedded_depth: usize) -> Result<NormalizedXmlDocument, String> {
    if xml.len() > MAX_XML_BYTES {
        return Err(format!(
            "XML exceeds the {}-byte normalization limit",
            MAX_XML_BYTES
        ));
    }
    let parsed = Document::parse_with_options(
        xml,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_XML_NODES,
            entity_resolver: None,
        },
    )
    .map_err(|error| format!("XML is malformed: {error}"))?;
    let mut counts = NormalizationCounts::default();
    let nodes = parsed
        .root()
        .children()
        .map(|node| normalize_node(node, embedded_depth, None, &mut counts))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(NormalizedXmlDocument {
        format_version: NORMALIZED_XML_FORMAT_VERSION,
        node_count: counts.node_count,
        embedded_document_count: counts.embedded_document_count,
        nodes,
    })
}

fn normalize_node(
    node: Node<'_, '_>,
    embedded_depth: usize,
    parent_name: Option<&str>,
    counts: &mut NormalizationCounts,
) -> Result<NormalizedXmlNode, String> {
    counts.node_count = counts
        .node_count
        .checked_add(1)
        .ok_or_else(|| "normalized XML node count overflowed".to_string())?;
    match node.node_type() {
        NodeType::Root => Err("an XML root node cannot be nested".to_string()),
        NodeType::Element => {
            let tag = node.tag_name();
            let name = tag.name().to_string();
            let namespace_uri = tag.namespace().map(str::to_string);
            let namespaces = node
                .namespaces()
                .map(|namespace| NormalizedXmlNamespace {
                    prefix: namespace.name().map(str::to_string),
                    uri: namespace.uri().to_string(),
                })
                .collect();
            let attributes = node
                .attributes()
                .map(|attribute| NormalizedXmlAttribute {
                    name: attribute.name().to_string(),
                    namespace_uri: attribute.namespace().map(str::to_string),
                    value: attribute.value().to_string(),
                })
                .collect();
            let children = node
                .children()
                .map(|child| normalize_node(child, embedded_depth, Some(&name), counts))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(NormalizedXmlNode::Element {
                name,
                namespace_uri,
                namespaces,
                attributes,
                children,
            })
        }
        NodeType::Text => {
            let value = node.text().unwrap_or_default().to_string();
            let embedded_document = normalize_embedded_text(&value, parent_name, embedded_depth)?;
            if let Some(document) = &embedded_document {
                counts.node_count = counts
                    .node_count
                    .checked_add(document.node_count)
                    .ok_or_else(|| "normalized XML node count overflowed".to_string())?;
                counts.embedded_document_count = counts
                    .embedded_document_count
                    .checked_add(1 + document.embedded_document_count)
                    .ok_or_else(|| "embedded XML document count overflowed".to_string())?;
            }
            Ok(NormalizedXmlNode::Text {
                value,
                embedded_document: embedded_document.map(Box::new),
            })
        }
        NodeType::Comment => Ok(NormalizedXmlNode::Comment {
            value: node.text().unwrap_or_default().to_string(),
        }),
        NodeType::PI => {
            let instruction = node
                .pi()
                .ok_or_else(|| "processing instruction payload is absent".to_string())?;
            Ok(NormalizedXmlNode::ProcessingInstruction {
                target: instruction.target.to_string(),
                value: instruction.value.map(str::to_string),
            })
        }
    }
}

fn normalize_embedded_text(
    value: &str,
    parent_name: Option<&str>,
    embedded_depth: usize,
) -> Result<Option<NormalizedXmlDocument>, String> {
    let trimmed = value.trim();
    let Some(parent_name) = parent_name else {
        return Ok(None);
    };
    let parent_name = parent_name.to_ascii_lowercase();
    let is_record_container = matches!(parent_name.as_str(), "recorditem" | "recordxml");
    let is_content_container = parent_name == "content";
    if (!is_record_container && !is_content_container)
        || !trimmed.starts_with('<')
        || !trimmed.ends_with('>')
    {
        return Ok(None);
    }
    // `<content>` is also used for ordinary text, URLs, and partially
    // escaped rich-media fields.  Only treat it as an embedded document when
    // it clearly advertises one of the document roots we understand.  A
    // malformed optional child is retained as text instead of poisoning the
    // otherwise valid outer message.  `recorditem`/`recordxml`, on the other
    // hand, are explicit document containers and remain strict.
    let looks_like_document = trimmed.starts_with("<msg")
        || trimmed.starts_with("<?xml")
        || trimmed.starts_with("<recordinfo")
        || trimmed.starts_with("<recordxml");
    if is_content_container && !looks_like_document {
        return Ok(None);
    }
    if embedded_depth >= MAX_EMBEDDED_DEPTH {
        return Err(format!(
            "embedded XML exceeds the depth limit of {MAX_EMBEDDED_DEPTH}"
        ));
    }
    match parse_document(trimmed, embedded_depth + 1) {
        Ok(document) => Ok(Some(document)),
        Err(_error) if is_content_container => Ok(None),
        Err(error) => Err(error),
    }
}

/// Remove an XML declaration that was incorrectly placed inside an outer
/// `<msg>` element.  A few real WeChat rows contain
/// `<msg><?xml version=...?><appmsg>...`, which is not well-formed XML even
/// though the declaration is clearly intended to describe the same payload.
/// We repair only that structural mistake for parsing/normalization and keep
/// the original bytes in `raw_xml`, so the archive remains lossless.  XML
/// declarations inside CDATA or comments are left untouched because those
/// are message text, not processing instructions.
fn repair_misplaced_xml_declarations(xml: &str) -> Cow<'_, str> {
    let lower = xml.to_ascii_lowercase();
    let Some(msg_start) = lower.find("<msg") else {
        return Cow::Borrowed(xml);
    };
    let Some(msg_open_end_relative) = find_tag_end(&xml[msg_start..]) else {
        return Cow::Borrowed(xml);
    };
    let msg_open_end = msg_start + msg_open_end_relative;
    let mut ranges = Vec::new();
    let mut search = msg_open_end.saturating_add(1);
    while let Some(relative) = lower[search..].find("<?xml") {
        let start = search + relative;
        // A declaration at the beginning of the document is valid and must
        // remain.  Only declarations after the outer `<msg>` opening are
        // candidates for repair.
        if start <= msg_open_end || inside_cdata_or_comment(&lower, start) {
            search = start.saturating_add(5);
            continue;
        }
        let Some(end_relative) = lower[start..].find("?>") else {
            break;
        };
        let end = start + end_relative + 2;
        ranges.push((start, end));
        search = end;
    }
    if ranges.is_empty() {
        return Cow::Borrowed(xml);
    }
    let mut repaired = String::with_capacity(xml.len());
    let mut cursor = 0;
    for (start, end) in ranges {
        repaired.push_str(&xml[cursor..start]);
        cursor = end;
    }
    repaired.push_str(&xml[cursor..]);
    Cow::Owned(repaired)
}

/// Apply all lossless-source-preserving repairs needed before XML parsing.
/// The returned projection may differ from the source only by removing
/// structural transport noise; callers always retain the original payload.
fn repair_xml_for_parsing(xml: &str) -> Cow<'_, str> {
    let declarations_repaired = repair_misplaced_xml_declarations(xml);
    match declarations_repaired {
        Cow::Borrowed(value) => repair_invalid_xml_characters(value),
        Cow::Owned(mut value) => {
            if value.chars().any(is_invalid_xml_character) {
                value.retain(|character| !is_invalid_xml_character(character));
            }
            Cow::Owned(value)
        }
    }
}

fn repair_invalid_xml_characters(xml: &str) -> Cow<'_, str> {
    if !xml.chars().any(is_invalid_xml_character) {
        return Cow::Borrowed(xml);
    }
    let mut repaired = String::with_capacity(xml.len());
    for character in xml.chars() {
        if !is_invalid_xml_character(character) {
            repaired.push(character);
        }
    }
    Cow::Owned(repaired)
}

fn is_invalid_xml_character(character: char) -> bool {
    !matches!(
        character as u32,
        0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF
    )
}

fn find_tag_end(xml: &str) -> Option<usize> {
    let mut quote = None;
    for (index, character) in xml.char_indices() {
        match (quote, character) {
            (Some(expected), value) if value == expected => quote = None,
            (None, '\'' | '"') => quote = Some(character),
            (None, '>') => return Some(index),
            _ => {}
        }
    }
    None
}

fn inside_cdata_or_comment(lower: &str, position: usize) -> bool {
    let cdata_start = lower[..position].rfind("<![cdata[");
    let cdata_end = lower[..position].rfind("]]>");
    if cdata_start.is_some_and(|start| cdata_end.is_none_or(|end| start > end)) {
        return true;
    }
    let comment_start = lower[..position].rfind("<!--");
    let comment_end = lower[..position].rfind("-->");
    comment_start.is_some_and(|start| comment_end.is_none_or(|end| start > end))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn canonical_nested_message(
        typed_payload: Value,
        semantic_decode_state: SemanticDecodeState,
        semantic_gap_reason: Option<String>,
    ) -> CanonicalMessage {
        CanonicalMessage {
            canonical_id: "message".to_string(),
            account_id: "account".to_string(),
            source_set_id: "set".to_string(),
            source_logical_path: "message.db".to_string(),
            source_table_id: "table".to_string(),
            source_table_name: "Msg_fixture".to_string(),
            source_row_id: 1,
            conversation_id: "conversation".to_string(),
            conversation_source_identifier_base64: "Y29udmVyc2F0aW9u".to_string(),
            sender_id: None,
            sender_source_identifier_base64: None,
            local_id: None,
            server_id: None,
            sort_sequence: None,
            created_at_unix: None,
            conversation_ordinal: 0,
            ordering_basis: crate::MessageOrderingBasis::HybridSourceFallback,
            raw_type: Some((19_i64 << 32) | 49_i64),
            logical_type: Some(49),
            sub_type: Some(19),
            status: None,
            direction: crate::MessageDirection::Unknown,
            direction_evidence: crate::DirectionEvidence::Unresolved,
            content_base64: None,
            packed_info_base64: None,
            compression_type: None,
            raw_columns: BTreeMap::new(),
            typed_payload: TypedPayload::Decoded(typed_payload),
            semantic_decode_state,
            semantic_gap_reason,
            relationships: Vec::new(),
            artifact_references: Vec::new(),
        }
    }

    #[test]
    fn normalizes_merged_children_and_embedded_xml_without_dropping_raw_fields() {
        let xml = r#"<msg><appmsg><type>19</type><title>Forwarded history</title><recorditem><![CDATA[<recordinfo><datalist><dataitem datatype="1" dataid="child-1"><sourcename>Alice</sourcename><sourcetime>2026-08-27</sourcetime><datadesc>Hello</datadesc></dataitem><dataitem datatype="49" dataid="child-2"><content>&lt;msg&gt;&lt;appmsg&gt;&lt;title&gt;Nested link&lt;/title&gt;&lt;/appmsg&gt;&lt;/msg&gt;</content></dataitem></datalist></recordinfo>]]></recorditem></appmsg></msg>"#;
        let content = wx_db::MessageContent::MergedMessages {
            title: Some("Forwarded history".to_string()),
            raw_xml: xml.to_string(),
        };
        let (value, gap) = serialize_message_content(&content).unwrap();
        assert!(gap.is_none());
        assert_eq!(value["MergedMessages"]["raw_xml"], xml);
        let normalized = &value["MergedMessages"]["normalized_xml"];
        assert_eq!(normalized["formatVersion"], 1);
        assert_eq!(normalized["embeddedDocumentCount"], 2);
        assert!(normalized["nodeCount"].as_u64().unwrap() > 10);
        assert!(serde_json::to_string(normalized)
            .unwrap()
            .contains("child-2"));
        assert!(serde_json::to_string(normalized)
            .unwrap()
            .contains("Nested link"));
    }

    #[test]
    fn normalizes_channel_media_graph_and_preserves_namespace_evidence() {
        let xml = r#"<msg xmlns:f="urn:finder"><appmsg><type>51</type><title>Clip</title><f:finderFeed id="feed-1"><objectId>123</objectId><mediaList><media><mediaType>4</mediaType><url>https://example.invalid/video</url><thumbUrl>https://example.invalid/thumb</thumbUrl><width>1080</width><height>1920</height></media></mediaList></f:finderFeed></appmsg></msg>"#;
        let content = wx_db::MessageContent::ChannelVideo {
            sub_type: 51,
            title: Some("Clip".to_string()),
            raw_xml: xml.to_string(),
        };
        let (value, gap) = serialize_message_content(&content).unwrap();
        assert!(gap.is_none());
        let normalized = &value["ChannelVideo"]["normalized_xml"];
        let encoded = serde_json::to_string(normalized).unwrap();
        assert!(encoded.contains("urn:finder"));
        assert!(encoded.contains("feed-1"));
        assert!(encoded.contains("thumbUrl"));
    }

    #[test]
    fn malformed_or_structurally_incomplete_nested_xml_remains_a_gap() {
        let malformed = wx_db::MessageContent::MergedMessages {
            title: None,
            raw_xml: "<msg><appmsg><recorditem><recordinfo>".to_string(),
        };
        let (value, gap) = serialize_message_content(&malformed).unwrap();
        assert!(gap
            .as_deref()
            .is_some_and(|reason| reason.contains("could not be normalized")));
        assert!(value["MergedMessages"].get("normalized_xml").is_none());

        let channel = wx_db::MessageContent::ChannelVideo {
            sub_type: 51,
            title: Some("title only".to_string()),
            raw_xml: "<msg><appmsg><title>title only</title></appmsg></msg>".to_string(),
        };
        let (_, gap) = serialize_message_content(&channel).unwrap();
        assert!(gap
            .as_deref()
            .is_some_and(|reason| reason.contains("Finder media graph is absent")));

        let malformed_child = wx_db::MessageContent::MergedMessages {
            title: None,
            raw_xml: r#"<msg><appmsg><recorditem><![CDATA[<recordinfo><datalist><dataitem><content>&lt;msg&gt;&lt;appmsg&gt;</content></dataitem></datalist></recordinfo>]]></recorditem></appmsg></msg>"#.to_string(),
        };
        let (value, gap) = serialize_message_content(&malformed_child).unwrap();
        // The outer merged-message graph is valid.  A malformed optional
        // child inside `<content>` is retained as text instead of poisoning
        // the complete outer projection.
        assert!(gap.is_none());
        let normalized = &value["MergedMessages"]["normalized_xml"];
        assert!(serde_json::to_string(normalized)
            .unwrap()
            .contains("<msg><appmsg>"));
    }

    #[test]
    fn repairs_misplaced_xml_declaration_without_mutating_raw_source() {
        let xml = r#"<msg><?xml version="1.0" encoding="utf-8"?><appmsg><type>19</type><recorditem><![CDATA[<recordinfo><datalist><dataitem datatype="1"><datadesc>child</datadesc></dataitem></datalist></recordinfo>]]></recorditem></appmsg></msg>"#;
        let content = wx_db::MessageContent::MergedMessages {
            title: None,
            raw_xml: xml.to_string(),
        };
        let (value, gap) = serialize_message_content(&content).unwrap();
        assert!(gap.is_none());
        assert_eq!(value["MergedMessages"]["raw_xml"], xml);
        assert!(value["MergedMessages"].get("normalized_xml").is_some());
    }

    #[test]
    fn generic_app_validation_accepts_valid_and_repairs_misplaced_declaration() {
        for xml in [
            r#"<msg><appmsg><type>24</type><title>generic</title></appmsg></msg>"#,
            r#"<msg><?xml version="1.0"?><appmsg><type>24</type><title>generic</title></appmsg></msg>"#,
        ] {
            let content = wx_db::MessageContent::AppGeneric {
                sub_type: 24,
                title: Some("generic".to_string()),
                des: None,
                url: None,
                raw_xml: xml.to_string(),
            };
            let (_, gap) = serialize_message_content(&content).unwrap();
            assert!(gap.is_none(), "unexpected generic XML gap: {gap:?}");
        }
    }

    #[test]
    fn declarations_inside_cdata_are_preserved_as_text() {
        let xml =
            r#"<msg><appmsg><![CDATA[<?xml version="1.0"?><msg><appmsg/></msg>]]></appmsg></msg>"#;
        // The nested declaration is message text inside CDATA; it must not be
        // stripped as if it were a structural processing instruction.
        let repaired = repair_misplaced_xml_declarations(xml);
        assert_eq!(repaired.as_ref(), xml);
    }

    #[test]
    fn extracts_inner_message_from_group_and_transport_prefixes() {
        let prefixed = "sender:\n<msg><appmsg><type>19</type><recorditem><![CDATA[<recordinfo><datalist/></recordinfo>]]></recorditem></appmsg></msg>";
        assert_eq!(
            xml_document_candidate(prefixed),
            Some("<msg><appmsg><type>19</type><recorditem><![CDATA[<recordinfo><datalist/></recordinfo>]]></recorditem></appmsg></msg>")
        );

        let wrapped = "<voipmsg type=\"VoIPBubbleMsg\"><msg><voipmsg><duration>1</duration></voipmsg></msg></voipmsg>";
        assert_eq!(
            xml_document_candidate(wrapped),
            Some("<msg><voipmsg><duration>1</duration></voipmsg></msg>")
        );
        assert!(xml_has_element_or_attribute(wrapped, "voipmsg"));
        assert!(normalize_xml_projection(wrapped).is_ok());
    }

    #[test]
    fn strips_forbidden_xml_control_characters_only_from_projection() {
        let xml = "<msg><appmsg><finderFeed><description>a\u{b}b</description></finderFeed></appmsg></msg>";
        let projected = normalize_xml_projection(xml).unwrap();
        let encoded = serde_json::to_string(&projected).unwrap();
        assert!(encoded.contains("ab"));
        assert!(!encoded.contains("\\u000b"));
        assert!(xml.contains('\u{b}'));
    }

    #[test]
    fn normalizes_only_the_observed_two_root_voip_fragment_shape() {
        let xml = concat!(
            "<voipinvitemsg><roomid>fixture</roomid></voipinvitemsg>",
            "<voipextinfo><recvtime>1</recvtime></voipextinfo>",
            "<voiplocalinfo><duration>1</duration></voiplocalinfo>"
        );
        let normalized = normalize_voip_xml_projection(xml).unwrap();
        assert_eq!(normalized["formatVersion"], 1);
        let encoded = serde_json::to_string(&normalized).unwrap();
        assert!(encoded.contains("greenbubbles-voip-fragments"));
        assert!(encoded.contains("voipinvitemsg"));
        assert!(encoded.contains("voipextinfo"));
        assert!(encoded.contains("voiplocalinfo"));

        assert!(
            normalize_voip_xml_projection("<voipinvitemsg/><unrelated-private-fragment/>").is_err()
        );
    }

    #[test]
    fn accepts_legacy_partial_records_without_weakening_complete_projection_checks() {
        let xml = r#"<msg><appmsg><type>19</type><recorditem><![CDATA[<recordinfo><datalist><dataitem datatype="1"><datadesc>Hello</datadesc></dataitem></datalist></recordinfo>]]></recorditem></appmsg></msg>"#;
        let content = wx_db::MessageContent::MergedMessages {
            title: None,
            raw_xml: xml.to_string(),
        };
        let (mut value, gap) = serialize_message_content(&content).unwrap();
        assert!(gap.is_none());
        value["MergedMessages"]
            .as_object_mut()
            .unwrap()
            .remove("normalized_xml");

        let legacy = canonical_nested_message(
            value.clone(),
            SemanticDecodeState::Partial,
            Some("merged-message children were not normalized".to_string()),
        );
        validate_canonical_message(&legacy).unwrap();

        let unjustified_complete =
            canonical_nested_message(value, SemanticDecodeState::Complete, None);
        assert!(validate_canonical_message(&unjustified_complete)
            .unwrap_err()
            .contains("lacks its reproducible projection"));
    }
}
