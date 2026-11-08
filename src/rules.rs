//! What counts as personal data: sensitive column names and value patterns.

use regex::Regex;
use std::sync::OnceLock;

fn column_rules() -> &'static [(Regex, &'static str)] {
    static RULES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    RULES.get_or_init(|| {
        [
            (r"(^|_)e?mail(_address)?$", "EMAIL"),
            (r"iban", "IBAN"),
            (r"phone|mobile", "PHONE"),
            (r"birth_?date|date_of_birth|birthday", "BIRTHDATE"),
            (r"place_?of_?birth|birth_?(place|city|country)", "BIRTHPLACE"),
            (r"(^|_)(address\w*|street\w*)$|(^|_)(city|postcode|zip_?code|postal_?code)$", "ADDRESS"),
            (r"nationality", "NATIONALITY"),
            (r"social_security|(^|_)nir$|(^|_)ssn$|tax_?(number|identification)", "SSN"),
            (r"(^|_)(first|last|full|birth|maiden|middle|given|family|holder|owner|beneficiary)_?names?$", "PERSON"),
            (r"embossed_?(legal_?)?name|name_?on_?card", "PERSON"),
            (r"^(firstname|lastname|fullname|surname|nom|prenom)$", "PERSON"),
            (r"(^|_)(organi[sz]ation|company|legal|business|trade|commercial)_?name$|^organi[sz]ation$", "ORG"),
        ]
        .into_iter()
        .map(|(pattern, kind)| (Regex::new(pattern).unwrap(), kind))
        .collect()
    })
}

pub fn column_kind(name: &str) -> Option<&'static str> {
    let name = name.trim().trim_matches(['"', ':']).to_lowercase();
    column_rules().iter().find(|(re, _)| re.is_match(&name)).map(|(_, kind)| *kind)
}

pub fn value_patterns() -> &'static [(Regex, &'static str)] {
    static RULES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    RULES.get_or_init(|| {
        [
            (r"[\w.+-]+@[\w-]+(\.[\w-]+)+", "EMAIL"),
            (r"\b[A-Z]{2}\d{2}(?: ?[A-Z0-9]{4}){2,7}(?: ?[A-Z0-9]{1,4})?\b", "IBAN"),
            (r"(?:\+33 ?|\b0)[1-9](?:[ .-]?\d{2}){4}\b", "PHONE"),
        ]
        .into_iter()
        .map(|(pattern, kind)| (Regex::new(pattern).unwrap(), kind))
        .collect()
    })
}

pub fn is_token(value: &str) -> bool {
    static TOKEN: OnceLock<Regex> = OnceLock::new();
    TOKEN.get_or_init(|| Regex::new(r"^[A-Z]+_[0-9a-f]{6}$").unwrap()).is_match(value)
}

pub fn is_nullish(value: &str) -> bool {
    matches!(value, "" | "NULL" | "null" | "nil" | "∅" | "(null)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_columns_get_their_kind() {
        let cases = [
            ("first_name", "PERSON"),
            ("last_name", "PERSON"),
            ("firstname", "PERSON"),
            ("middleNames", "PERSON"),
            ("beneficiary_name", "PERSON"),
            ("embossedName", "PERSON"),
            ("email", "EMAIL"),
            ("\"email\"", "EMAIL"),
            ("email_address", "EMAIL"),
            ("iban", "IBAN"),
            ("phone_number", "PHONE"),
            ("mobile", "PHONE"),
            ("birth_date", "BIRTHDATE"),
            ("birthday", "BIRTHDATE"),
            ("placeOfBirth", "BIRTHPLACE"),
            ("birthCountry", "BIRTHPLACE"),
            ("address", "ADDRESS"),
            ("address1", "ADDRESS"),
            ("street", "ADDRESS"),
            ("city", "ADDRESS"),
            ("postcode", "ADDRESS"),
            ("zip_code", "ADDRESS"),
            ("nationality", "NATIONALITY"),
            ("social_security_number", "SSN"),
            ("taxNumber", "SSN"),
            ("organisation_name", "ORG"),
            ("legalName", "ORG"),
            ("company_name", "ORG"),
        ];
        for (column, kind) in cases {
            assert_eq!(column_kind(column), Some(kind), "{column}");
        }
    }

    #[test]
    fn useful_columns_stay_visible() {
        for column in [
            "id",
            "name",
            "state",
            "status",
            "amount",
            "country",
            "bic",
            "kyc_level",
            "user_id",
            "userStatus",
            "created_at",
        ] {
            assert_eq!(column_kind(column), None, "{column}");
        }
    }

    #[test]
    fn value_patterns_catch_contact_data() {
        let kinds = |text: &str| {
            value_patterns().iter().filter(|(re, _)| re.is_match(text)).map(|(_, k)| *k).collect::<Vec<_>>()
        };
        assert_eq!(kinds("jean.dupont@example.com"), ["EMAIL"]);
        assert_eq!(kinds("FR76 3000 6000 0112 3456 7890 189"), ["IBAN"]);
        assert_eq!(kinds("FR7630006000011234567890189"), ["IBAN"]);
        assert_eq!(kinds("06 12 34 56 78"), ["PHONE"]);
        assert_eq!(kinds("+33612345678"), ["PHONE"]);
        assert!(kinds("amount 1234.56, id 42").is_empty());
    }

    #[test]
    fn tokens_and_nulls_are_recognized() {
        assert!(is_token("PERSON_d86896"));
        assert!(!is_token("PERSON_d86896x"));
        assert!(!is_token("Jean"));
        assert!(is_nullish("NULL") && is_nullish("") && is_nullish("nil"));
    }
}
