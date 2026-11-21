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

fn digits(text: &str) -> Vec<u32> {
    text.chars().filter_map(|c| c.to_digit(10)).collect()
}

/// Card numbers: Luhn checksum, and either groups of four (`4242 4242 4242 4242`) or 15/16 joined digits
/// starting like a card network (3 Amex, 4 Visa, 5 and 2 Mastercard, 6 Discover). Millisecond timestamps
/// (13 digits from 1) and SIRET numbers (14 digits) do not qualify.
fn plausible_card(text: &str) -> bool {
    let numbers = digits(text);
    let grouped = text.contains([' ', '-'])
        && text.split([' ', '-']).all(|group| !group.is_empty())
        && text.split([' ', '-']).rev().skip(1).all(|group| group.len() == 4);
    let joined = !text.contains([' ', '-']) && matches!(numbers.len(), 15 | 16) && "23456".contains(&text[..1]);
    let luhn = numbers
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &d)| if i % 2 == 1 { if d * 2 > 9 { d * 2 - 9 } else { d * 2 } } else { d })
        .sum::<u32>()
        % 10
        == 0;
    (13..=19).contains(&numbers.len()) && (grouped || joined) && luhn
}

/// French social security numbers (NIR): the last two digits are 97 minus the first thirteen modulo 97,
/// with Corsica's 2A and 2B counted as 19 and 18.
fn plausible_nir(text: &str) -> bool {
    let compact: String = text.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if compact.len() != 15 {
        return false;
    }
    let (body, key) = compact.split_at(13);
    let body = body.replace("2A", "19").replace("2B", "18");
    let offset = if compact[5..7].eq("2A") {
        1_000_000
    } else if compact[5..7].eq("2B") {
        2_000_000
    } else {
        0
    };
    match (body.parse::<u64>(), key.parse::<u64>()) {
        (Ok(body), Ok(key)) => 97 - (body - offset) % 97 == key,
        _ => false,
    }
}

/// International phone numbers written with their `+` prefix: 8 to 15 digits, as E.164 allows.
fn plausible_international_phone(text: &str) -> bool {
    (8..=15).contains(&digits(text).len())
}

/// Public IPv4 addresses only: private networks, loopback and link-local addresses name no one and are what
/// infrastructure debugging needs to read.
fn public_ipv4(text: &str) -> bool {
    text.parse::<std::net::Ipv4Addr>()
        .is_ok_and(|ip| !(ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified()))
}

/// IPv6 candidates are checked by the standard parser: clock times, MAC addresses and fingerprints fail it.
/// A digit is required too, so a Ruby or Rust path made of hex letters (`Db::Cafe`) stays readable.
/// As for IPv4, loopback, link-local (`fe80::`) and unique local (`fd00::`) addresses stay readable.
fn public_ipv6(text: &str) -> bool {
    text.chars().any(|c| c.is_ascii_digit())
        && text.parse::<std::net::Ipv6Addr>().is_ok_and(|ip| {
            !(ip.is_loopback() || ip.is_unspecified() || ip.is_unicast_link_local() || ip.is_unique_local())
        })
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
        // `%40` is the `@` of an email inside a URL (`?email=jean%40example.com` in request logs)
        value(r"[\w.+-]+(?:@|%40)[\w-]+(\.[\w-]+)+", "EMAIL"),
        ValueRule {
            pattern: r"\b[12][ .]?\d{2}[ .]?\d{2}[ .]?(?:\d{2}|2[AB])[ .]?\d{3}[ .]?\d{3}[ .]?\d{2}\b",
            kind: "SSN",
            accepts: Some(plausible_nir),
        },
        // digits all separated or all joined: a mix is more likely a piece of an identifier
        value(r"(?:\+33[ .]?|\b0)[1-9](?:(?:[ .-]\d{2}){4}|\d{8})\b", "PHONE"),
        // other countries only with their `+` prefix: without it, too many technical numbers look alike;
        // +33 is left to the French rule above
        ValueRule {
            pattern: r"\+(?:[124-9]\d{0,2}|3[0-24-9]\d?)(?:[ .-]?\(?\d{1,4}\)?){2,5}\b",
            kind: "PHONE",
            accepts: Some(plausible_international_phone),
        },
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
    values: &[
        ValueRule {
            pattern: r"\b[A-Z]{2}\d{2}(?: ?[A-Z0-9]{4}){2,7}(?: ?[A-Z0-9]{1,4})?\b",
            kind: "IBAN",
            accepts: Some(plausible_iban),
        },
        ValueRule { pattern: r"\b\d(?:[ -]?\d){12,18}\b", kind: "CARD", accepts: Some(plausible_card) },
    ],
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
    values: &[
        ValueRule {
            pattern: r"\b(?:(?:25[0-5]|2[0-4]\d|1?\d?\d)\.){3}(?:25[0-5]|2[0-4]\d|1?\d?\d)\b",
            kind: "IP",
            accepts: Some(public_ipv4),
        },
        // a whole run of hex groups and colons, so that a fingerprint is not cut into a valid address
        ValueRule { pattern: r"(?i)\b[0-9a-f]{1,4}(?::+[0-9a-f]{1,4})+\b", kind: "IP", accepts: Some(public_ipv6) },
        // a JWT is a credential, and its payload carries the user's email and name in base64
        value(r"\beyJ[\w-]+\.eyJ[\w-]+\.[\w-]*", "SECRET"),
    ],
    commands: &[],
};

// payments first: an IBAN must be matched before the phone pattern can bite into it
pub const SETS: &[RuleSet] = &[PAYMENTS, COMMON, COLLABORATION, SECURITY, DATABASES];
