//! Entity-relationship diagrams. mmdflux has no ER support, so the diagram is
//! translated into an equivalent class diagram (entities become cards whose
//! members are the attributes, relationships become edges labeled with their
//! cardinalities) and rendered through the class-diagram pipeline.

use mmdflux::{render_diagram, OutputFormat, RenderConfig};
use std::fmt::Write;

use super::{fits_width, render_class_cards, render_horizontal_class_diagram, ClassBlock};
use crate::markdown::width::iter_cluster_widths;

/// Stand-in for `)` inside attributes: mmdflux files any member containing
/// `)` under the methods compartment, which would split `varchar(255) name`
/// away from the other attributes.
const CLOSE_PAREN_PLACEHOLDER: char = '\u{E000}';

pub(super) fn render(content: &str, max_width: usize) -> Option<String> {
    let diagram = parse(content)?;
    let source = diagram.to_class_source();

    let rendered = render_diagram(&source, OutputFormat::Text, &RenderConfig::default())
        .ok()
        .filter(|rendered| fits_width(rendered, max_width))
        .or_else(|| render_horizontal_class_diagram(&source, max_width));
    if let Some(rendered) = rendered {
        return Some(remove_empty_compartments(
            &rendered.replace(CLOSE_PAREN_PLACEHOLDER, ")"),
        ));
    }

    let cards: Vec<ClassBlock> = diagram
        .entities
        .iter()
        .map(|entity| ClassBlock {
            name: entity.display_name().to_string(),
            members: entity.attributes.clone(),
        })
        .collect();
    let relationships: Vec<String> = diagram
        .relationships
        .iter()
        .map(|rel| diagram.describe(rel))
        .collect();
    render_class_cards(&cards, &relationships, max_width)
}

#[derive(Debug, Default)]
struct Entity {
    id: String,
    alias: Option<String>,
    attributes: Vec<String>,
}

impl Entity {
    fn display_name(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.id)
    }
}

#[derive(Debug)]
struct Relationship {
    from: String,
    to: String,
    from_cardinality: &'static str,
    to_cardinality: &'static str,
    identifying: bool,
    label: String,
}

#[derive(Debug, Default)]
struct ErDiagram {
    direction: Option<String>,
    entities: Vec<Entity>,
    relationships: Vec<Relationship>,
}

impl ErDiagram {
    fn entity_mut(&mut self, id: &str) -> &mut Entity {
        let index = match self.entities.iter().position(|entity| entity.id == id) {
            Some(index) => index,
            None => {
                self.entities.push(Entity {
                    id: id.to_string(),
                    ..Entity::default()
                });
                self.entities.len() - 1
            }
        };
        &mut self.entities[index]
    }

    fn declare(&mut self, declaration: &str) -> Option<&mut Entity> {
        let (id, alias) = parse_entity_declaration(declaration)?;
        let entity = self.entity_mut(&id);
        if alias.is_some() {
            entity.alias = alias;
        }
        Some(entity)
    }

    fn display_name<'a>(&'a self, id: &'a str) -> &'a str {
        self.entities
            .iter()
            .find(|entity| entity.id == id)
            .map_or(id, Entity::display_name)
    }

    fn describe(&self, rel: &Relationship) -> String {
        let line = if rel.identifying { "──" } else { "┄┄" };
        let mut text = format!(
            "{} ({}) {line}",
            self.display_name(&rel.from),
            rel.from_cardinality
        );
        if !rel.label.is_empty() {
            let _ = write!(text, " {} {line}", rel.label);
        }
        let _ = write!(
            text,
            " ({}) {}",
            rel.to_cardinality,
            self.display_name(&rel.to)
        );
        text
    }

    fn to_class_source(&self) -> String {
        let mut out = String::from("classDiagram\n");
        if let Some(direction) = &self.direction {
            let _ = writeln!(out, "    direction {direction}");
        }
        for entity in &self.entities {
            let _ = write!(out, "    class {}", entity.id);
            if let Some(alias) = &entity.alias {
                let _ = write!(out, "[\"{alias}\"]");
            }
            if entity.attributes.is_empty() {
                out.push('\n');
                continue;
            }
            out.push_str(" {\n");
            for attribute in &entity.attributes {
                let attribute = attribute.replace(')', &CLOSE_PAREN_PLACEHOLDER.to_string());
                let _ = writeln!(out, "        {attribute}");
            }
            out.push_str("    }\n");
        }
        for rel in &self.relationships {
            let line = if rel.identifying { "--" } else { ".." };
            let _ = write!(
                out,
                "    {} \"{}\" {line} \"{}\" {}",
                rel.from, rel.from_cardinality, rel.to_cardinality, rel.to
            );
            if !rel.label.is_empty() {
                let _ = write!(out, " : {}", rel.label);
            }
            out.push('\n');
        }
        out
    }
}

fn parse(content: &str) -> Option<ErDiagram> {
    let mut lines = content.lines();
    let header = lines.next()?.trim();
    if header != "erDiagram" {
        return None;
    }

    let mut diagram = ErDiagram::default();
    let mut current: Option<String> = None;

    for source_line in lines {
        let line = source_line.trim();
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }

        if let Some(id) = current.as_deref() {
            if line == "}" {
                current = None;
            } else if let Some(attribute) = parse_attribute(line) {
                let id = id.to_string();
                diagram.entity_mut(&id).attributes.push(attribute);
            }
            continue;
        }

        let keyword = line.split_whitespace().next().unwrap_or_default();
        match keyword {
            "direction" => {
                diagram.direction = line.split_whitespace().nth(1).map(str::to_string);
                continue;
            }
            "title" | "accTitle" | "accTitle:" | "accDescr" | "accDescr:" | "style"
            | "classDef" | "class" => continue,
            _ => {}
        }

        if let Some(declaration) = line.strip_suffix('{') {
            let id = diagram.declare(declaration.trim())?.id.clone();
            current = Some(id);
            continue;
        }

        if let Some(rel) = parse_relationship(line) {
            diagram.entity_mut(&rel.from);
            diagram.entity_mut(&rel.to);
            diagram.relationships.push(rel);
            continue;
        }

        diagram.declare(line)?;
    }

    if diagram.entities.is_empty() {
        return None;
    }
    Some(diagram)
}

/// `NAME`, `NAME[Alias]` or `NAME["Alias"]`.
fn parse_entity_declaration(declaration: &str) -> Option<(String, Option<String>)> {
    let (id, alias) = match declaration.split_once('[') {
        Some((id, rest)) => {
            let alias = rest.strip_suffix(']')?.trim().trim_matches('"').trim();
            (id.trim(), (!alias.is_empty()).then(|| alias.to_string()))
        }
        None => (declaration, None),
    };
    is_entity_name(id).then(|| (id.to_string(), alias))
}

fn is_entity_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

/// `type name [PK, FK] ["comment"]`, normalized to single spaces.
fn parse_attribute(line: &str) -> Option<String> {
    let (body, comment) = match line.find('"') {
        Some(start) => (&line[..start], Some(line[start..].trim())),
        None => (line, None),
    };
    let mut attribute = body.split_whitespace().collect::<Vec<_>>().join(" ");
    attribute = attribute.replace(" ,", ",");
    if attribute.is_empty() {
        return None;
    }
    if let Some(comment) = comment {
        attribute.push(' ');
        attribute.push_str(comment);
    }
    Some(attribute)
}

fn parse_relationship(line: &str) -> Option<Relationship> {
    let (spec, label) = match line.split_once(':') {
        Some((spec, label)) => (spec.trim(), label.trim().trim_matches('"').trim()),
        None => (line, ""),
    };
    let tokens: Vec<&str> = spec.split_whitespace().collect();
    if tokens.len() < 3 {
        return None;
    }
    let from = *tokens.first()?;
    let to = *tokens.last()?;
    if !is_entity_name(from) || !is_entity_name(to) {
        return None;
    }

    let (from_cardinality, identifying, to_cardinality) = if tokens.len() == 3 {
        parse_symbolic_relationship(tokens[1])?
    } else {
        parse_word_relationship(&tokens[1..tokens.len() - 1])?
    };

    Some(Relationship {
        from: from.to_string(),
        to: to.to_string(),
        from_cardinality,
        to_cardinality,
        identifying,
        label: label.to_string(),
    })
}

/// Crow's-foot notation such as `||--o{` or `}|..|{`.
fn parse_symbolic_relationship(token: &str) -> Option<(&'static str, bool, &'static str)> {
    if token.len() != 6 || !token.is_ascii() {
        return None;
    }
    let from = match &token[..2] {
        "|o" => "0..1",
        "||" => "1",
        "}o" => "0..*",
        "}|" => "1..*",
        _ => return None,
    };
    let identifying = match &token[2..4] {
        "--" => true,
        ".." | ".-" | "-." => false,
        _ => return None,
    };
    let to = match &token[4..] {
        "o|" => "0..1",
        "||" => "1",
        "o{" => "0..*",
        "|{" => "1..*",
        _ => return None,
    };
    Some((from, identifying, to))
}

/// Word aliases such as `one or more to zero or one` or
/// `1 optionally to many(0)`.
fn parse_word_relationship(words: &[&str]) -> Option<(&'static str, bool, &'static str)> {
    let phrase = words.join(" ").to_ascii_lowercase();
    let (from, identifying, to) = if let Some((from, to)) = phrase.split_once(" optionally to ") {
        (from, false, to)
    } else {
        let (from, to) = phrase.split_once(" to ")?;
        (from, true, to)
    };
    Some((word_cardinality(from)?, identifying, word_cardinality(to)?))
}

fn word_cardinality(phrase: &str) -> Option<&'static str> {
    match phrase.trim() {
        "one or zero" | "zero or one" => Some("0..1"),
        "one or more" | "one or many" | "many(1)" | "1+" => Some("1..*"),
        "zero or more" | "zero or many" | "many(0)" | "0+" => Some("0..*"),
        "only one" | "exactly one" | "1" => Some("1"),
        _ => None,
    }
}

/// mmdflux always draws an operations compartment on class cards that have
/// members, which is empty for ER entities. Collapse each `├───┤` row that
/// sits directly on a `└───┘` into the card's bottom border, keeping any edge
/// that leaves from the old bottom border connected.
fn remove_empty_compartments(rendered: &str) -> String {
    // One cell per terminal column; wide clusters are followed by an empty
    // continuation cell so columns line up across rows.
    let mut grid: Vec<Vec<String>> = rendered
        .lines()
        .map(|line| {
            let mut cells = Vec::new();
            for (cluster, width) in iter_cluster_widths(line) {
                cells.push(cluster.to_string());
                for _ in 1..width {
                    cells.push(String::new());
                }
            }
            cells
        })
        .collect();

    let cell = |grid: &[Vec<String>], row: usize, col: usize| -> Option<String> {
        grid.get(row)?.get(col).cloned()
    };

    for row in 0..grid.len().saturating_sub(1) {
        let mut col = 0;
        while col < grid[row].len() {
            if grid[row][col] != "├" || cell(&grid, row + 1, col).as_deref() != Some("└") {
                col += 1;
                continue;
            }
            let mut end = col + 1;
            while cell(&grid, row, end).as_deref() == Some("─")
                && cell(&grid, row + 1, end).as_deref() == Some("─")
            {
                end += 1;
            }
            if cell(&grid, row, end).as_deref() != Some("┤")
                || cell(&grid, row + 1, end).as_deref() != Some("┘")
            {
                col += 1;
                continue;
            }

            grid[row][col] = "└".to_string();
            grid[row][end] = "┘".to_string();
            for c in col..=end {
                let below = cell(&grid, row + 2, c).unwrap_or_default();
                grid[row + 1][c] = match below.as_str() {
                    "│" | "┆" | "┊" | "╎" | "┼" | "┬" | "▲" | "▼" | "△" | "▽" | "◆" | "◇" => {
                        "│".to_string()
                    }
                    _ => " ".to_string(),
                };
            }
            col = end + 1;
        }
    }

    let lines: Vec<String> = grid
        .iter()
        .map(|cells| cells.concat().trim_end().to_string())
        .collect();
    let mut out = lines.join("\n");
    out.truncate(out.trim_end().len());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "erDiagram
    CUSTOMER ||--o{ ORDER : places
    ORDER ||--|{ LINE-ITEM : contains
    CUSTOMER }|..|{ DELIVERY-ADDRESS : uses
    CUSTOMER {
        string name
        string custNumber PK \"customer id\"
    }
    ORDER {
        int orderNumber PK
        varchar(255) deliveryAddress
    }
";

    #[test]
    fn parses_entities_attributes_and_relationships() {
        let diagram = parse(SAMPLE).unwrap();
        let ids: Vec<&str> = diagram.entities.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["CUSTOMER", "ORDER", "LINE-ITEM", "DELIVERY-ADDRESS"]);
        assert_eq!(
            diagram.entities[0].attributes,
            ["string name", "string custNumber PK \"customer id\""]
        );
        let rel = &diagram.relationships[2];
        assert_eq!(
            (rel.from_cardinality, rel.identifying, rel.to_cardinality),
            ("1..*", false, "1..*")
        );
        assert_eq!(rel.label, "uses");
    }

    #[test]
    fn parses_word_cardinalities_and_aliases() {
        let diagram = parse(
            "erDiagram\n  p[\"Person\"] {\n    string name\n  }\n  p only one optionally to zero or more CAR : \"drives\"\n",
        )
        .unwrap();
        assert_eq!(diagram.entities[0].display_name(), "Person");
        let rel = &diagram.relationships[0];
        assert_eq!(
            (rel.from_cardinality, rel.identifying, rel.to_cardinality),
            ("1", false, "0..*")
        );
        assert_eq!(rel.label, "drives");
        assert_eq!(diagram.describe(rel), "Person (1) ┄┄ drives ┄┄ (0..*) CAR");
    }

    #[test]
    fn rejects_unparseable_lines() {
        assert!(parse("erDiagram\n  A ||--?? B\n").is_none());
        assert!(parse("erDiagram\n").is_none());
    }

    #[test]
    fn renders_graph_with_cardinalities_and_no_empty_compartments() {
        let rendered = render(SAMPLE, 200).unwrap();
        for expected in [
            "CUSTOMER",
            "LINE-ITEM",
            "string custNumber PK \"customer id\"",
            "varchar(255) deliveryAddress",
            "places",
            "0..*",
            "1..*",
        ] {
            assert!(
                rendered.contains(expected),
                "missing {expected}:\n{rendered}"
            );
        }
        let lines: Vec<&str> = rendered.lines().collect();
        for pair in lines.windows(2) {
            let tees = pair[0]
                .match_indices('├')
                .map(|(i, _)| pair[0][..i].chars().count());
            for col in tees {
                assert_ne!(
                    pair[1].chars().nth(col),
                    Some('└'),
                    "empty compartment left behind:\n{rendered}"
                );
            }
        }
        assert!(!rendered.contains(CLOSE_PAREN_PLACEHOLDER));
    }

    #[test]
    fn falls_back_to_cards_when_graph_is_too_wide() {
        let rendered = render(SAMPLE, 40).unwrap();
        assert!(rendered
            .lines()
            .all(|line| super::super::display_width(line) <= 40));
        assert!(rendered.contains("Relationships"), "{rendered}");
        assert!(
            rendered.contains("CUSTOMER (1) ── places ── (0..*) ORDER"),
            "{rendered}"
        );
    }

    #[test]
    fn remove_empty_compartments_keeps_edges_connected() {
        let input = "┌───┐\n│ A │\n├───┤\n│ x │\n├───┤\n└───┘\n  │\n┌───┐\n│ B │\n└───┘";
        assert_eq!(
            remove_empty_compartments(input),
            "┌───┐\n│ A │\n├───┤\n│ x │\n└───┘\n  │\n  │\n┌───┐\n│ B │\n└───┘"
        );
    }
}
