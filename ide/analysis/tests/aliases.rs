//! Include aliases in the editor: a module or a name brought in under another
//! name with `=>`, names brought in alone with braces, and variants named
//! through their kind (`Shape.Circle`). In the sources below, `@@` marks the
//! cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::collections::HashMap;

use vela_analysis::{Analysis, LineIndex, TokenClass};

const MAIN: &str = "\
include shop/prices => shop (Item => Product, price_of)
include fuel/prices {Size => Fuel, price_of => fuel_price}

task main()
    CHECK
    fixed tea: Product = Product { name: \"tea\", cents: 250 }
    fixed big = shop.Size.Large(75)
    fixed tank = Fuel.Tank(40)
    fixed can = Fuel.Small
    when tank is Fuel.Tank liters
        show(fuel_price(liters))
    fi
    match big
    case shop.Size.Large extra
        show(extra)
    case Small
        show(price_of(tea, shop.Size.Small))
    fi
    @@
fi
";

const SHOP: &str = "\
shape Item
    name: text
    cents: int
fi

kind Size
    Small
    Large(extra: int)
fi

task price_of(item: Item, size: Size) -> int
    <- item.cents
fi
";

const FUEL: &str = "\
kind Size
    Small
    Tank(liters: int)
fi

task price_of(liters: int) -> int
    <- liters * 180
fi
";

fn sources() -> HashMap<String, String> {
    [("app/shop/prices.vela", SHOP), ("app/fuel/prices.vela", FUEL)].into_iter().map(|(path, source)| (path.to_string(), source.to_string())).collect()
}

/// `MAIN` with `code` at the cursor, and a first line in `main` that either
/// checks or stops the checker before anything else in `main`.
fn program(checks: bool, code: &str) -> String {
    let first = if checks { "show(0)" } else { "show(unknown)" };
    MAIN.replace("CHECK", first).replace("    @@\n", &format!("    {}\n", code))
}

fn analysis(source: &str) -> Analysis {
    static SOURCES: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();
    Analysis::with_modules(source, "app/main.vela", SOURCES.get_or_init(sources))
}

/// The UTF-16 offset of the first occurrence of `needle` in `source`.
fn at(source: &str, needle: &str) -> usize {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    LineIndex::new(source).utf16_of(byte)
}

fn class_at(source: &str, needle: &str) -> TokenClass {
    let offset = at(source, needle);
    analysis(source).tokens().into_iter().find(|t| t.from == offset).unwrap_or_else(|| panic!("no token at {:?}", needle)).class
}

fn hover(source: &str, needle: &str) -> (String, String) {
    let hover = analysis(source).hover(at(source, needle)).unwrap_or_else(|| panic!("no hover at {:?}", needle));
    (hover.code, hover.doc)
}

fn labels(marked: &str) -> Vec<String> {
    let offset = marked.find("@@").unwrap();
    let source = marked.replacen("@@", "", 1);
    analysis(&source).complete(LineIndex::new(&source).utf16_of(offset)).items.into_iter().map(|item| item.label).collect()
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    let source = program(true, "show(0)");
    assert_eq!(analysis(&source).diagnostics(), Vec::new());
}

#[test]
fn names_on_include_lines_and_through_kinds_are_highlighted_as_what_they_are() {
    let source = program(true, "show(0)");
    assert_eq!(class_at(&source, "shop ("), TokenClass::Module);
    assert_eq!(class_at(&source, "Item =>"), TokenClass::Type);
    assert_eq!(class_at(&source, "=> Product"), TokenClass::Operator);
    assert_eq!(class_at(&source, "Product,"), TokenClass::Type);
    assert_eq!(class_at(&source, "{Size"), TokenClass::Punctuation);
    assert_eq!(class_at(&source, "fuel_price}"), TokenClass::Task);
    assert_eq!(class_at(&source, "Fuel.Tank(40)"), TokenClass::Type);
    assert_eq!(class_at(&source, "Tank(40)"), TokenClass::Variant);
    assert_eq!(class_at(&source, "Size.Large(75)"), TokenClass::Type);
    assert_eq!(class_at(&source, "Large(75)"), TokenClass::Variant);
    assert_eq!(class_at(&source, "Large extra"), TokenClass::Variant);
    assert_eq!(class_at(&source, "extra\n"), TokenClass::Variable);
    assert_eq!(class_at(&source, "Tank liters"), TokenClass::Variant);
}

#[test]
fn hover_names_things_as_the_file_writes_them() {
    for checks in [true, false] {
        let source = program(checks, "show(0)");
        let code = |needle: &str| hover(&source, needle).0;
        let when = if checks { "checked" } else { "declared" };
        assert_eq!(code("tea:"), "bind tea: Product", "{}", when);
        assert_eq!(code("big ="), "bind big: shop.Size", "{}", when);
        assert_eq!(code("tank ="), "bind tank: Fuel", "{}", when);
        assert_eq!(code("can ="), "bind can: Fuel", "{}", when);
        assert_eq!(code("liters\n"), "liters: int", "{}", when);
        assert_eq!(code("extra\n"), "extra: int", "{}", when);
    }
    let source = program(true, "show(0)");
    assert_eq!(hover(&source, "Product {").0, "shape Product\n    name: text\n    cents: int\nfi");
    assert_eq!(hover(&source, "price_of(tea").0, "task shop.price_of(item: Product, size: shop.Size) -> int");
    assert_eq!(hover(&source, "fuel_price(").0, "task fuel_price(liters: int) -> int");
    assert_eq!(hover(&source, "Tank(40)"), ("Fuel.Tank(liters: int)".to_string(), "A variant of kind Fuel. From module fuel/prices (app/fuel/prices.vela).".to_string()));
    assert_eq!(hover(&source, "shop (").0, "include shop/prices => shop");
}

#[test]
fn definitions_lead_into_the_modules() {
    let source = program(true, "show(0)");
    let analysis = analysis(&source);
    assert_eq!(analysis.definition(at(&source, "Product {")).unwrap().path.as_deref(), Some("app/shop/prices.vela"));
    assert_eq!(analysis.definition(at(&source, "Fuel.Tank(40)")).unwrap().path.as_deref(), Some("app/fuel/prices.vela"));
    assert_eq!(analysis.definition(at(&source, "Tank(40)")).unwrap().path.as_deref(), Some("app/fuel/prices.vela"));
    assert_eq!(analysis.definition(at(&source, "fuel_price(")).unwrap().path.as_deref(), Some("app/fuel/prices.vela"));
}

#[test]
fn completion_offers_names_as_the_file_writes_them() {
    for checks in [true, false] {
        // Through a module's name: not what is brought in under another name.
        let members = labels(&program(checks, "show(shop.@@)"));
        assert!(members.contains(&"Size".to_string()) && members.contains(&"price_of".to_string()), "{:?}", members);
        assert!(!members.contains(&"Item".to_string()), "{:?}", members);
        // Through a kind's name: its variants.
        assert_eq!(labels(&program(checks, "show(Fuel.@@)")), ["Small", "Tank"]);
        assert_eq!(labels(&program(checks, "show(shop.Size.@@)")), ["Small", "Large"]);
        // Values: the names the file writes, not those an alias or braces hide.
        let values = labels(&program(checks, "show(@@)"));
        for name in ["shop", "price_of", "fuel_price", "tea"] {
            assert!(values.contains(&name.to_string()), "{} in {:?}", name, values);
        }
        assert!(!values.iter().any(|name| name == "prices" || name == "fuel/prices"), "{:?}", values);
    }
    // In braces, what the module declares, by its own names; nothing for an alias.
    let listed = labels(&MAIN.replace("{Size => Fuel, price_of => fuel_price}", "{Size => Fuel, @@}"));
    assert_eq!(listed, ["Size", "Small", "Tank", "price_of"]);
    assert_eq!(labels(&MAIN.replace("{Size => Fuel,", "{Size => @@")), Vec::<String>::new());
}

#[test]
fn messages_name_things_as_the_file_writes_them() {
    let messages = |code: &str| analysis(&program(true, code)).diagnostics().into_iter().map(|d| d.message).collect::<Vec<_>>();
    assert_eq!(messages("fixed n: int = tea"), ["expected type int, got Product"]);
    assert_eq!(messages("fixed n: int = tank"), ["expected type int, got Fuel"]);
    assert_eq!(messages("fixed n: int = big"), ["expected type int, got shop.Size"]);
    assert_eq!(messages("show(Item)"), ["unknown variable 'Item'; it is brought in as 'Product' at 1:38"]);
    // A name before a dot that is neither a module nor a kind is a value with a
    // method, so an unknown one is reported as the value it would be.
    assert_eq!(messages("show(prices.price_of(tea, big))"), ["unknown variable 'prices'; module 'shop/prices' is included as 'shop'"]);
}

#[test]
fn every_request_works_on_every_prefix_of_a_program_with_aliases() {
    // Half-typed include lines and `Kind.Variant`s, cut at every character.
    let full = program(true, "show(0)");
    for cut in full.char_indices().map(|(i, _)| i) {
        let source = &full[..cut];
        let analysis = analysis(source);
        let _ = (analysis.tokens(), analysis.diagnostics(), analysis.outline(), analysis.missing_modules());
        let length = LineIndex::new(source).utf16_of(source.len());
        for offset in (0..=length).step_by(5) {
            let _ = (analysis.complete(offset), analysis.hover(offset), analysis.definition(offset));
        }
    }
}

#[test]
fn patterns_through_a_kind_offer_its_variants_not_yet_matched() {
    for checks in [true, false] {
        let when = if checks { "checked" } else { "declared" };
        // After `case Fuel.`, the variants of the kind, less the cases already written,
        // however they are written.
        assert_eq!(labels(&program(checks, "match tank\n    case Fuel.@@\n    fi")), ["Small", "Tank"], "{}", when);
        let one_written = "match tank\n    case Fuel.Tank n\n        show(n)\n    case Fuel.@@\n    fi";
        assert_eq!(labels(&program(checks, one_written)), ["Small"], "{}", when);
        let alone = "match tank\n    case Tank n\n        show(n)\n    case Fuel.@@\n    fi";
        assert_eq!(labels(&program(checks, alone)), ["Small"], "{}", when);
        // After `case`, the same.
        let through_kind = "match tank\n    case Fuel.Tank n\n        show(n)\n    case @@\n    fi";
        assert_eq!(labels(&program(checks, through_kind)), ["Small", "_"], "{}", when);
        let through_module = "match big\n    case shop.Size.Large n\n        show(n)\n    case @@\n    fi";
        assert_eq!(labels(&program(checks, through_module)), ["Small", "_"], "{}", when);
        // Every variant after `is Fuel.`, which covers nothing.
        assert_eq!(labels(&program(checks, "when tank is Fuel.@@")), ["Small", "Tank"], "{}", when);
        // A shape or a misspelled kind has no variants.
        assert_eq!(labels(&program(checks, "match tank\n    case Product.@@\n    fi")), Vec::<String>::new(), "{}", when);
        assert_eq!(labels(&program(checks, "when tank is Fual.@@")), Vec::<String>::new(), "{}", when);
    }
}

#[test]
fn definitions_of_names_on_include_lines_and_through_modules() {
    let source = program(true, "show(0)");
    let analysis = analysis(&source);
    let shop = Some("app/shop/prices.vela");
    let fuel = Some("app/fuel/prices.vela");
    let definition = |needle: &str| analysis.definition(at(&source, needle)).map(|location| (location.path, location.from));
    let declared = |module: &str, needle: &str| LineIndex::new(module).utf16_of(module.find(needle).unwrap());
    // A module's alias leads to its file, where it is used and on its include line.
    assert_eq!(definition("shop.Size.Large"), Some((shop.map(str::to_string), 0)));
    assert_eq!(definition("shop ("), Some((shop.map(str::to_string), 0)));
    // A kind through its module, and a variant through both.
    assert_eq!(definition("Size.Large(75)"), Some((shop.map(str::to_string), declared(SHOP, "Size\n"))));
    assert_eq!(definition("Large(75)"), Some((shop.map(str::to_string), declared(SHOP, "Large("))));
    // The original name left of `=>` leads to the declaration, as the alias does.
    assert_eq!(definition("Item =>"), Some((shop.map(str::to_string), declared(SHOP, "Item\n"))));
    assert_eq!(definition("Product,"), Some((shop.map(str::to_string), declared(SHOP, "Item\n"))));
    assert_eq!(definition("Size => Fuel"), Some((fuel.map(str::to_string), declared(FUEL, "Size\n"))));
    assert_eq!(definition("price_of => fuel_price"), Some((fuel.map(str::to_string), declared(FUEL, "price_of("))));
    // A variant named alone in a case is the matched kind's.
    assert_eq!(definition("Small\n        show(price_of"), Some((shop.map(str::to_string), declared(SHOP, "Small\n"))));
    // A folder of an include path declares nothing.
    assert_eq!(definition("fuel/prices"), None);
}
