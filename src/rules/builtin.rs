//! Built-in rules, grouped by the kind of source they come from.
//! Column patterns match lowercased column or key names, and `parent.key` inside JSON documents.

pub struct RuleSet {
    pub name: &'static str,
    /// Names that stay visible even when a column rule of any built-in set matches.
    pub keep: &'static [&'static str],
    pub columns: &'static [(&'static str, &'static str)],
    pub values: &'static [ValueRule],
    /// Shell commands whose output the Claude Code hook masks.
    pub commands: &'static [&'static str],
}

pub struct ValueRule {
    pub pattern: &'static str,
    pub kind: &'static str,
    /// Extra check on a regex match, for formats a regex alone gets wrong.
    pub accepts: Option<fn(&str) -> bool>,
}

const fn value(pattern: &'static str, kind: &'static str) -> ValueRule {
    ValueRule { pattern, kind, accepts: None }
}

/// Countries of the IBAN registry.
const IBAN_COUNTRIES: &str = "AD AE AL AT AZ BA BE BG BH BI BR BY CH CR CY CZ DE DJ DK DO EE EG ES FI FK FO FR GB GE \
    GI GL GR GT HR HU IE IL IQ IS IT JO KW KZ LB LC LI LT LU LV LY MC MD ME MK MN MR MT MU NI NL NO OM PK PL PS PT QA \
    RO RS RU SA SC SD SE SI SK SM SO ST SV TL TN TR UA VA VG XK YE";

/// Uppercase references (`AB12CDEF3456`) look like IBANs: require a real country code and a real length.
fn plausible_iban(text: &str) -> bool {
    let compact_len = text.chars().filter(|c| !c.is_whitespace()).count();
    (15..=34).contains(&compact_len) && IBAN_COUNTRIES.split_whitespace().any(|country| text.starts_with(country))
}

/// Identity and contact data, whatever the source.
const COMMON: RuleSet = RuleSet {
    name: "common",
    // identifiers, timestamps and flags about a personal field are not the field itself
    keep: &[r"_(ids?|at|count|type|status)$"],
    columns: &[
        (r"(^|_)e?mail(_address|_contact)?$", "EMAIL"),
        (r"(^|_)(phone|mobile|telephone)(_?(number|no))?(_contact)?$", "PHONE"),
        (r"birth_?date|date_of_birth|birthday|(^|_)dob$", "BIRTHDATE"),
        (r"place_?of_?birth|birth_?(place|city|country)", "BIRTHPLACE"),
        (r"(^|_)(address\w*|street\w*)$|(^|_)(city|postcode|zip_?code|postal_?code)$", "ADDRESS"),
        (r"nationality", "NATIONALITY"),
        (r"social_security|(^|_)nir$|(^|_)ssn$|tax_?(number|identification)|(^|_)id_number$", "SSN"),
        (
            r"(^|_)(first|last|full|birth|maiden|middle|given|family|holder|owner|beneficiary|author|contact|employee)_?names?$",
            "PERSON",
        ),
        (r"^(firstname|lastname|fullname|surname|nom|prenom)$", "PERSON"),
        (r"(^|_)(organi[sz]ation|company|corporate|legal|business|trade|commercial)_?name$|^organi[sz]ation$", "ORG"),
    ],
    values: &[
        value(r"[\w.+-]+@[\w-]+(\.[\w-]+)+", "EMAIL"),
        // digits all separated or all joined: a mix is more likely a piece of an identifier
        value(r"(?:\+33[ .]?|\b0)[1-9](?:(?:[ .-]\d{2}){4}|\d{8})\b", "PHONE"),
    ],
    commands: &[],
};

/// Databases and production consoles.
const DATABASES: RuleSet = RuleSet {
    name: "databases",
    keep: &[],
    columns: &[],
    values: &[],
    commands: &["psql", "heroku run", "heroku logs"],
};

/// Banking and payments: SEPA transfers, card issuers (Treezor…), Stripe.
const PAYMENTS: RuleSet = RuleSet {
    name: "payments",
    keep: &[],
    columns: &[
        (r"(^|_)iban(_number|_bidx|_ciphertext)?$", "IBAN"),
        (
            r"(^|_)(creditor|debtor|debitor|recipient|payer|payee|issuer|customer|cardholder|account_?holder)_?name$",
            "PERSON",
        ),
        (r"embossed_?(legal_?)?name|name_?on_?card", "PERSON"),
        // Stripe objects: `name` is a person inside these parents
        (r"(^|\.)(billing_details|shipping|customer|individual|person|cardholder|account_holder)\.name$", "PERSON"),
    ],
    values: &[ValueRule {
        pattern: r"\b[A-Z]{2}\d{2}(?: ?[A-Z0-9]{4}){2,7}(?: ?[A-Z0-9]{1,4})?\b",
        kind: "IBAN",
        accepts: Some(plausible_iban),
    }],
    commands: &["stripe"],
};

/// Collaboration tools: Notion, Slack, GitHub user objects.
const COLLABORATION: RuleSet = RuleSet {
    name: "collaboration",
    keep: &[],
    columns: &[
        (r"(^|\.)user\.name$", "PERSON"),
        // Slack: `real_name`, `display_name_normalized`, `userName`
        (r"(^|_)(real|display|user)_?name(_normalized)?$", "PERSON"),
    ],
    values: &[],
    commands: &[],
};

/// Network identifiers and credentials.
const SECURITY: RuleSet = RuleSet {
    name: "security",
    keep: &[],
    columns: &[
        (r"(^|_)ip(_address)?$", "IP"),
        (
            r"(^|_)(encrypted_)?password$|secret|(^|_)(api_?key|access_?token|refresh_?token|reset_password_token|invitation_token|unlock_token)$",
            "SECRET",
        ),
    ],
    values: &[value(r"\b(?:(?:25[0-5]|2[0-4]\d|1?\d?\d)\.){3}(?:25[0-5]|2[0-4]\d|1?\d?\d)\b", "IP")],
    commands: &[],
};

// payments first: an IBAN must be matched before the phone pattern can bite into it
pub const SETS: &[RuleSet] = &[PAYMENTS, COMMON, COLLABORATION, SECURITY, DATABASES];
