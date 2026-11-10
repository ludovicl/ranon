//! Built-in rules, grouped by the kind of source they come from.
//! Column patterns match lowercased column or key names, and `parent.key` inside JSON documents.

pub struct RuleSet {
    pub name: &'static str,
    pub columns: &'static [(&'static str, &'static str)],
    pub values: &'static [(&'static str, &'static str)],
    /// Shell commands whose output the Claude Code hook masks.
    pub commands: &'static [&'static str],
}

/// Identity and contact data, whatever the source.
const COMMON: RuleSet = RuleSet {
    name: "common",
    columns: &[
        (r"(^|_)e?mail(_address|_contact)?$", "EMAIL"),
        (r"phone|mobile", "PHONE"),
        (r"birth_?date|date_of_birth|birthday|(^|_)dob$", "BIRTHDATE"),
        (r"place_?of_?birth|birth_?(place|city|country)", "BIRTHPLACE"),
        (r"(^|_)(address\w*|street\w*)$|(^|_)(city|postcode|zip_?code|postal_?code)$", "ADDRESS"),
        (r"nationality", "NATIONALITY"),
        (r"social_security|(^|_)nir$|(^|_)ssn$|tax_?(number|identification)|(^|_)id_number$", "SSN"),
        (r"(^|_)(first|last|full|birth|maiden|middle|given|family|holder|owner|beneficiary)_?names?$", "PERSON"),
        (r"^(firstname|lastname|fullname|surname|nom|prenom)$", "PERSON"),
        (r"(^|_)(organi[sz]ation|company|corporate|legal|business|trade|commercial)_?name$|^organi[sz]ation$", "ORG"),
    ],
    values: &[(r"[\w.+-]+@[\w-]+(\.[\w-]+)+", "EMAIL"), (r"(?:\+33 ?|\b0)[1-9](?:[ .-]?\d{2}){4}\b", "PHONE")],
    commands: &[],
};

/// Databases and production consoles.
const DATABASES: RuleSet = RuleSet { name: "databases", columns: &[], values: &[], commands: &["psql", "heroku run"] };

/// Banking and payments: SEPA transfers, card issuers (Treezor…), Stripe.
const PAYMENTS: RuleSet = RuleSet {
    name: "payments",
    columns: &[
        (r"iban", "IBAN"),
        (
            r"(^|_)(creditor|debtor|debitor|recipient|payer|payee|issuer|customer|cardholder|account_?holder)_?name$",
            "PERSON",
        ),
        (r"embossed_?(legal_?)?name|name_?on_?card", "PERSON"),
        // Stripe objects: `name` is a person inside these parents
        (r"(^|\.)(billing_details|shipping|customer|individual|person|cardholder|account_holder)\.name$", "PERSON"),
    ],
    values: &[(r"\b[A-Z]{2}\d{2}(?: ?[A-Z0-9]{4}){2,7}(?: ?[A-Z0-9]{1,4})?\b", "IBAN")],
    commands: &["stripe"],
};

/// Collaboration tools: Notion, Slack, GitHub user objects.
const COLLABORATION: RuleSet =
    RuleSet { name: "collaboration", columns: &[(r"(^|\.)user\.name$", "PERSON")], values: &[], commands: &[] };

/// Network identifiers and credentials.
const SECURITY: RuleSet = RuleSet {
    name: "security",
    columns: &[
        (r"(^|_)ip(_address)?$", "IP"),
        (
            r"(^|_)(encrypted_)?password$|secret|(^|_)(api_?key|access_?token|refresh_?token|reset_password_token|invitation_token|unlock_token)$",
            "SECRET",
        ),
    ],
    values: &[(r"\b(?:(?:25[0-5]|2[0-4]\d|1?\d?\d)\.){3}(?:25[0-5]|2[0-4]\d|1?\d?\d)\b", "IP")],
    commands: &[],
};

// payments first: an IBAN must be matched before the phone pattern can bite into it
pub const SETS: &[RuleSet] = &[PAYMENTS, COMMON, COLLABORATION, SECURITY, DATABASES];
