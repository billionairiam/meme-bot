use std::{collections::HashMap, str::FromStr};

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
        let register_body = self
            .product_category_body("REGISTER")
            .ok_or_else(|| DomainError::Parse("missing REGISTER pricing category".to_string()))?;
        Self::price_for_years_in_category(register_body, tld, years)
    }

    fn product_category_body(&self, category_name: &str) -> Option<&'a str> {
        let mut cursor = 0;
        while let Some(start) = self.xml[cursor..].find("<ProductCategory ") {
            let absolute_start = cursor + start;
            let Some(end) = self.xml[absolute_start..].find('>') else {
                break;
            };
            let category_tag = &self.xml[absolute_start..absolute_start + end + 1];
            let category_attrs = Self::parse_attrs(category_tag);
            let name = category_attrs
                .get("Name")
                .map(|value| value.to_ascii_uppercase());

            let category_close = "</ProductCategory>";
            let content_start = absolute_start + end + 1;
            let Some(close_offset) = self.xml[content_start..].find(category_close) else {
                break;
            };
            let category_body = &self.xml[content_start..content_start + close_offset];

            if name.as_deref() == Some(category_name) {
                return Some(category_body);
            }
            cursor = content_start + close_offset + category_close.len();
        }

        None
    }

    fn price_for_years_in_category(
        category_body: &str,
        tld: &str,
        years: i32,
    ) -> Result<Decimal, DomainError> {
        let mut cursor = 0;
        while let Some(start) = category_body[cursor..].find("<Product ") {
            let absolute_start = cursor + start;
            let Some(end) = category_body[absolute_start..].find('>') else {
                break;
            };
            let product_tag = &category_body[absolute_start..absolute_start + end + 1];
            let product_attrs = Self::parse_attrs(product_tag);
            let product_name = product_attrs
                .get("Name")
                .map(|value| value.trim_start_matches('.').to_ascii_lowercase());

            let product_close = "</Product>";
            let content_start = absolute_start + end + 1;
            let Some(close_offset) = category_body[content_start..].find(product_close) else {
                break;
            };
            let product_body = &category_body[content_start..content_start + close_offset];

            if product_name.as_deref() == Some(tld) {
                return Self::price_in_product(product_body, years);
            }
            cursor = content_start + close_offset + product_close.len();
        }

        Err(DomainError::Parse(format!("missing pricing for .{tld}")))
    }

    fn price_in_product(product_body: &str, years: i32) -> Result<Decimal, DomainError> {
        let mut cursor = 0;
        while let Some(start) = product_body[cursor..].find("<Price ") {
            let absolute_start = cursor + start;
            let Some(end) = product_body[absolute_start..].find('>') else {
                break;
            };
            let tag = &product_body[absolute_start..absolute_start + end + 1];
            let attrs = Self::parse_attrs(tag);
            let duration = attrs
                .get("Duration")
                .and_then(|value| value.parse::<i32>().ok());
            let duration_type = attrs
                .get("DurationType")
                .map(|value| value.to_ascii_uppercase());

            if duration == Some(years) && duration_type.as_deref() == Some("YEAR") {
                return attrs.decimal("Price");
            }
            cursor = absolute_start + end + 1;
        }

        Err(DomainError::Parse(format!(
            "missing {years} year registration price"
        )))
    }

    pub(super) fn first_tag_text(&self, tag: &str) -> Option<String> {
        let open_start = self.xml.find(&format!("<{tag}"))?;
        let open_end = self.xml[open_start..].find('>')? + open_start;
        let close_start = self.xml[open_end + 1..].find(&format!("</{tag}>"))? + open_end + 1;
        Some(self.xml[open_end + 1..close_start].trim().to_string())
    }

    pub(super) fn first_tag_attrs(&self, tag: &str) -> Option<NamecheapXmlAttrs> {
        let start = self.xml.find(&format!("<{tag} "))?;
        let end = self.xml[start..].find('>')? + start;
        Some(Self::parse_attrs(&self.xml[start..=end]))
    }

    pub(super) fn parse_attrs(tag: &str) -> NamecheapXmlAttrs {
        let mut attrs = HashMap::new();
        let bytes = tag.as_bytes();
        let mut index = 0;

        while index < bytes.len() {
            while index < bytes.len() && !bytes[index].is_ascii_alphabetic() {
                index += 1;
            }
            let key_start = index;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric()
                    || bytes[index] == b'_'
                    || bytes[index] == b'-')
            {
                index += 1;
            }
            if key_start == index {
                break;
            }
            let key = &tag[key_start..index];

            while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            if index >= bytes.len() || bytes[index] != b'=' {
                continue;
            }
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            if index >= bytes.len() || bytes[index] != b'"' {
                continue;
            }
            index += 1;
            let value_start = index;
            while index < bytes.len() && bytes[index] != b'"' {
                index += 1;
            }
            if index <= bytes.len() {
                attrs.insert(key.to_string(), Self::unescape(&tag[value_start..index]));
            }
            index += 1;
        }

        NamecheapXmlAttrs { attrs }
    }

    pub(super) fn domain_parts(domain: &str) -> Result<(String, String), DomainError> {
        let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
        let (sld, tld) = domain.rsplit_once('.').ok_or(DomainError::UnsupportedTld)?;
        if sld.is_empty() || tld.is_empty() {
            return Err(DomainError::UnsupportedTld);
        }
        Ok((sld.to_string(), tld.to_string()))
    }

    pub(super) fn unescape(value: &str) -> String {
        value
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&")
    }
}

pub(super) struct NamecheapXmlAttrs {
    attrs: HashMap<String, String>,
}

impl NamecheapXmlAttrs {
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
