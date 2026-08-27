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
        wx_db::MessageContent::AppGeneric { sub_type, .. } => {
            return Ok((
                value,
                Some(format!(
                    "app message subtype {sub_type} has only generic XML decoding"
                )),
            ));
        }
        _ => return Ok((value, None)),
    };
    match kind {
        Ok(()) => Ok((value, None)),
        Err(reason) => Ok((value, Some(reason))),
    }
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
    let document = parse_document(raw_xml, 0)?;
    let parsed = Document::parse_with_options(
        raw_xml,
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
    let is_embedded_container = parent_name.is_some_and(|name| {
        matches!(
            name.to_ascii_lowercase().as_str(),
            "recorditem" | "content" | "recordxml"
        )
    });
    let trimmed = value.trim();
    if !is_embedded_container || !trimmed.starts_with('<') || !trimmed.ends_with('>') {
        return Ok(None);
    }
    if embedded_depth >= MAX_EMBEDDED_DEPTH {
        return Err(format!(
            "embedded XML exceeds the depth limit of {MAX_EMBEDDED_DEPTH}"
        ));
    }
    parse_document(trimmed, embedded_depth + 1).map(Some)
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
        assert!(gap
            .as_deref()
            .is_some_and(|reason| reason.contains("could not be normalized")));
        assert!(value["MergedMessages"].get("normalized_xml").is_none());
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
