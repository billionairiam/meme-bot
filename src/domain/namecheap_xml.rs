use std::{collections::HashMap, str::FromStr};

use quick_xml::{
    Reader,
    escape::unescape,
    events::{BytesStart, Event},
};
use rust_decimal::Decimal;

use super::DomainError;

pub(super) struct NamecheapXml<'a> {
    xml: &'a str,
}

impl<'a> NamecheapXml<'a> {
    pub(super) fn new(xml: &'a str) -> Self {
        Self { xml }
    }

    pub(super) fn price_for_years(&self, tld: &str, years: i32) -> Result<Decimal, DomainError> {
        let mut reader = Reader::from_str(self.xml);
        reader.config_mut().trim_text(true);

        let tld = tld.trim_start_matches('.').to_ascii_lowercase();
        let mut in_register_category = false;
        let mut in_target_product = false;
        let mut saw_register_category = false;
        let mut saw_target_product = false;

        loop {
            match reader.read_event() {
                Ok(Event::Start(element)) => {
                    let attrs = Self::attrs_from_start(&reader, &element)?;
                    if element.name().as_ref() == b"ProductCategory" {
                        if attrs
                            .get("Name")
                            .is_some_and(|name| name.eq_ignore_ascii_case("REGISTER"))
                        {
                            in_register_category = true;
                            saw_register_category = true;
                        }
                    } else if in_register_category
                        && element.name().as_ref() == b"Product"
                        && product_name_matches(&attrs, &tld)
                    {
                        in_target_product = true;
                        saw_target_product = true;
                    } else if in_target_product
                        && element.name().as_ref() == b"Price"
                        && price_duration_matches(&attrs, years)
                    {
                        return attrs.decimal("Price");
                    }
                }
                Ok(Event::Empty(element)) => {
                    let attrs = Self::attrs_from_start(&reader, &element)?;
                    if element.name().as_ref() == b"ProductCategory" {
                        if attrs
                            .get("Name")
                            .is_some_and(|name| name.eq_ignore_ascii_case("REGISTER"))
                        {
                            saw_register_category = true;
                        }
                    } else if in_register_category
                        && element.name().as_ref() == b"Product"
                        && product_name_matches(&attrs, &tld)
                    {
                        saw_target_product = true;
                    } else if in_target_product
                        && element.name().as_ref() == b"Price"
                        && price_duration_matches(&attrs, years)
                    {
                        return attrs.decimal("Price");
                    }
                }
                Ok(Event::End(element)) => {
                    if element.name().as_ref() == b"Product" {
                        in_target_product = false;
                    } else if element.name().as_ref() == b"ProductCategory" {
                        in_register_category = false;
                    }
                }
                Ok(Event::Eof) => break,
                Ok(_) => {}
                Err(error) => return Err(DomainError::Parse(error.to_string())),
            }
        }

        if !saw_register_category {
            return Err(DomainError::Parse(
                "missing REGISTER pricing category".to_string(),
            ));
        }
        if !saw_target_product {
            return Err(DomainError::Parse(format!("missing pricing for .{tld}")));
        }
        Err(DomainError::Parse(format!(
            "missing {years} year registration price"
        )))
    }

    pub(super) fn first_tag_text(&self, tag: &str) -> Option<String> {
        let mut reader = Reader::from_str(self.xml);
        reader.config_mut().trim_text(true);
        let tag = tag.as_bytes();
        let mut collecting = false;
        let mut depth = 0usize;
        let mut text = String::new();

        loop {
            match reader.read_event().ok()? {
                Event::Start(element) if element.name().as_ref() == tag => {
                    collecting = true;
                    depth = 1;
                }
                Event::Start(_) if collecting => depth += 1,
                Event::Text(event) if collecting => {
                    let decoded = event.decode().ok()?;
                    let unescaped = unescape(&decoded).ok()?;
                    text.push_str(unescaped.trim());
                }
                Event::CData(event) if collecting => {
                    let decoded = event.decode().ok()?;
                    text.push_str(decoded.trim());
                }
                Event::End(element) if collecting && element.name().as_ref() == tag => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(text);
                    }
                }
                Event::End(_) if collecting => depth = depth.saturating_sub(1),
                Event::Eof => return None,
                _ => {}
            }
        }
    }

    pub(super) fn first_tag_attrs(&self, tag: &str) -> Option<NamecheapXmlAttrs> {
        let mut reader = Reader::from_str(self.xml);
        reader.config_mut().trim_text(true);
        let tag = tag.as_bytes();

        loop {
            match reader.read_event().ok()? {
                Event::Start(element) | Event::Empty(element) if element.name().as_ref() == tag => {
                    return Self::attrs_from_start(&reader, &element).ok();
                }
                Event::Eof => return None,
                _ => {}
            }
        }
    }

    #[cfg(test)]
    pub(super) fn parse_attrs(tag: &str) -> NamecheapXmlAttrs {
        let mut reader = Reader::from_str(tag);
        reader.config_mut().trim_text(true);
        loop {
            match reader.read_event() {
                Ok(Event::Start(element)) | Ok(Event::Empty(element)) => {
                    return Self::attrs_from_start(&reader, &element)
                        .unwrap_or_else(|_| NamecheapXmlAttrs::empty());
                }
                Ok(Event::Eof) | Err(_) => return NamecheapXmlAttrs::empty(),
                Ok(_) => {}
            }
        }
    }

    pub(super) fn domain_parts(domain: &str) -> Result<(String, String), DomainError> {
        let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
        let (sld, tld) = domain.rsplit_once('.').ok_or(DomainError::UnsupportedTld)?;
        if sld.is_empty() || tld.is_empty() {
            return Err(DomainError::UnsupportedTld);
        }
        Ok((sld.to_string(), tld.to_string()))
    }

    #[cfg(test)]
    pub(super) fn unescape(value: &str) -> String {
        unescape(value)
            .map(|value| value.into_owned())
            .unwrap_or_else(|_| value.to_string())
    }

    fn attrs_from_start(
        reader: &Reader<&[u8]>,
        element: &BytesStart<'_>,
    ) -> Result<NamecheapXmlAttrs, DomainError> {
        let mut attrs = HashMap::new();
        for attr in element.attributes() {
            let attr = attr.map_err(|error| DomainError::Parse(error.to_string()))?;
            let key = std::str::from_utf8(attr.key.as_ref())
                .map_err(|error| DomainError::Parse(error.to_string()))?
                .to_string();
            let value = attr
                .decode_and_unescape_value(reader.decoder())
                .map_err(|error| DomainError::Parse(error.to_string()))?
                .into_owned();
            attrs.insert(key, value);
        }
        Ok(NamecheapXmlAttrs { attrs })
    }
}

pub(super) struct NamecheapXmlAttrs {
    attrs: HashMap<String, String>,
}

impl NamecheapXmlAttrs {
    #[cfg(test)]
    fn empty() -> Self {
        Self {
            attrs: HashMap::new(),
        }
    }

    pub(super) fn get(&self, name: &str) -> Option<&String> {
        self.attrs.get(name)
    }

    pub(super) fn bool(&self, name: &str) -> Result<bool, DomainError> {
        self.get(name)
            .map(|value| matches!(value.to_ascii_lowercase().as_str(), "true" | "yes" | "1"))
            .ok_or_else(|| DomainError::Parse(format!("missing {name}")))
    }

    pub(super) fn decimal(&self, name: &str) -> Result<Decimal, DomainError> {
        let value = self
            .get(name)
            .ok_or_else(|| DomainError::Parse(format!("missing {name}")))?;
        Decimal::from_str(value).map_err(|error| DomainError::Parse(error.to_string()))
    }

    pub(super) fn required(&self, name: &str) -> Result<String, DomainError> {
        self.get(name)
            .filter(|value| !value.trim().is_empty())
            .cloned()
            .ok_or_else(|| DomainError::Parse(format!("missing {name}")))
    }
}

fn product_name_matches(attrs: &NamecheapXmlAttrs, tld: &str) -> bool {
    attrs
        .get("Name")
        .map(|value| value.trim_start_matches('.').to_ascii_lowercase())
        .as_deref()
        == Some(tld)
}

fn price_duration_matches(attrs: &NamecheapXmlAttrs, years: i32) -> bool {
    let duration = attrs
        .get("Duration")
        .and_then(|value| value.parse::<i32>().ok());
    let duration_type = attrs
        .get("DurationType")
        .map(|value| value.to_ascii_uppercase());

    duration == Some(years) && duration_type.as_deref() == Some("YEAR")
}
