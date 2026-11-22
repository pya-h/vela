use vela_analysis::{LineIndex, Token, TokenClass};

pub const TOKEN_TYPES: &[&str] = &[
    "keyword",      // 0
    "type",         // 1
    "function",     // 2
    "method",       // 3
    "parameter",    // 4
    "variable",     // 5
    "property",     // 6
    "number",       // 7
    "string",       // 8
    "comment",      // 9
    "operator",     // 10
    "namespace",    // 11
    "enumMember",   // 12
];

pub const TOKEN_MODIFIERS: &[&str] = &[
    "declaration",    // 1 << 0
    "readonly",       // 1 << 1
    "defaultLibrary", // 1 << 2
];

pub fn class_to_type(class: TokenClass) -> Option<u32> {
    match class {
        TokenClass::Keyword => Some(0),
        TokenClass::TypeBuiltin | TokenClass::Type => Some(1),
        TokenClass::Task | TokenClass::BuiltinFn => Some(2),
        TokenClass::Method => Some(3),
        TokenClass::Parameter => Some(4),
        TokenClass::Variable | TokenClass::Region => Some(5),
        TokenClass::Field => Some(6),
        TokenClass::Number => Some(7),
        TokenClass::String | TokenClass::Escape => Some(8),
        TokenClass::Comment => Some(9),
        TokenClass::Operator | TokenClass::Punctuation => Some(10),
        TokenClass::Module => Some(11),
        TokenClass::Variant => Some(12),
        TokenClass::Invalid => None,
    }
}

pub fn class_to_modifiers(class: TokenClass) -> u32 {
    match class {
        TokenClass::TypeBuiltin | TokenClass::BuiltinFn => 1 << 2, // defaultLibrary
        _ => 0,
    }
}

pub fn encode_tokens(tokens: &[Token], index: &LineIndex<'_>) -> Vec<u32> {
    let mut prev_line = 0;
    let mut prev_col = 0;
    let mut data = Vec::with_capacity(tokens.len() * 5);

    for token in tokens {
        let Some(type_idx) = class_to_type(token.class) else {
            continue;
        };
        let (line, col) = index.line_col_utf16(token.from);
        let length = token.to.saturating_sub(token.from);
        if length == 0 {
            continue;
        }

        let delta_line = line.saturating_sub(prev_line);
        let delta_col = if delta_line == 0 {
            col.saturating_sub(prev_col)
        } else {
            col
        };

        let modifiers = class_to_modifiers(token.class);

        let Ok(delta_line) = u32::try_from(delta_line) else { continue };
        let Ok(delta_col) = u32::try_from(delta_col) else { continue };
        let Ok(length) = u32::try_from(length) else { continue };

        data.push(delta_line);
        data.push(delta_col);
        data.push(length);
        data.push(type_idx);
        data.push(modifiers);

        prev_line = line;
        prev_col = col;
    }

    data
}
