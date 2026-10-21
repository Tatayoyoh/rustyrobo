//! Splits a shell script into top-level statements so that each statement's
//! result can be shown separately, like Robo 3T does.
//!
//! Splitting happens on `;` and on newlines at bracket depth 0, unless the
//! statement obviously continues on the next line (method chaining, binary
//! operators, `else`, …). Strings, template literals, comments and regex
//! literals are skipped.

#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    pub text: String,
    /// 0-based line of the first character, for error reporting.
    pub line: usize,
}

const CONTINUE_AFTER: &[char] = &[
    ',', '(', '[', '{', '=', '+', '-', '*', '/', '%', '&', '|', '^', '?', ':', '.', '<', '>', '!', '~',
];
const CONTINUE_BEFORE: &[char] = &[
    '.', ',', ')', ']', '}', '?', ':', '+', '*', '/', '%', '&', '|', '^', '=', '<', '>',
];
const CONTINUE_KEYWORDS: &[&str] = &["else", "catch", "finally", "while"];
const OPEN_KEYWORDS: &[&str] = &[
    "return", "typeof", "instanceof", "in", "of", "new", "delete", "void", "throw", "case", "do",
    "else", "await", "yield",
];

pub fn split_statements(src: &str) -> Vec<Statement> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut start = 0usize;
    let mut i = 0usize;
    let mut line = 0usize;
    let mut start_line = 0usize;
    // Last significant char (outside comments/whitespace), for regex detection
    // and continuation rules.
    let mut last_sig: Option<char> = None;
    let mut last_word = String::new();

    let push = |out: &mut Vec<Statement>, from: usize, to: usize, line: usize| {
        let text: String = chars[from..to].iter().collect();
        let trimmed = text.trim();
        let trimmed = trimmed.trim_end_matches(';').trim();
        if !trimmed.is_empty() {
            // Account for leading blank lines inside the chunk.
            let lead = text.len() - text.trim_start().len();
            let skipped = text[..lead].matches('\n').count();
            out.push(Statement { text: trimmed.to_string(), line: line + skipped });
        }
    };

    while i < chars.len() {
        let c = chars[i];
        match c {
            '\n' => {
                line += 1;
                if depth <= 0 && should_split_at_newline(&chars, i + 1, last_sig, &last_word) {
                    push(&mut out, start, i, start_line);
                    start = i + 1;
                    start_line = line;
                    last_sig = None;
                    last_word.clear();
                }
                i += 1;
                continue;
            }
            '/' if chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                    if chars[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
                i += 2;
                continue;
            }
            '"' | '\'' | '`' => {
                let q = c;
                i += 1;
                let mut tdepth = 0;
                while i < chars.len() {
                    let d = chars[i];
                    if d == '\\' {
                        i += 2;
                        continue;
                    }
                    if d == '\n' {
                        line += 1;
                        if q != '`' {
                            break;
                        }
                    }
                    if q == '`' && d == '$' && chars.get(i + 1) == Some(&'{') {
                        tdepth += 1;
                        i += 2;
                        continue;
                    }
                    if q == '`' && d == '}' && tdepth > 0 {
                        tdepth -= 1;
                    } else if d == q && tdepth == 0 {
                        break;
                    }
                    i += 1;
                }
                i += 1;
                last_sig = Some(q);
                last_word.clear();
                continue;
            }
            '/' if regex_allowed(last_sig, &last_word) => {
                i += 1;
                let mut in_class = false;
                while i < chars.len() && chars[i] != '\n' {
                    match chars[i] {
                        '\\' => i += 1,
                        '[' => in_class = true,
                        ']' => in_class = false,
                        '/' if !in_class => break,
                        _ => {}
                    }
                    i += 1;
                }
                i += 1;
                while i < chars.len() && chars[i].is_ascii_alphabetic() {
                    i += 1;
                }
                last_word.clear();
                // Mark as an operand so a following `/` is division.
                last_sig = Some('x');
                continue;
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ';' if depth <= 0 => {
                push(&mut out, start, i + 1, start_line);
                start = i + 1;
                start_line = line;
                last_sig = None;
                last_word.clear();
                i += 1;
                continue;
            }
            _ => {}
        }
        if c.is_alphanumeric() || c == '_' || c == '$' {
            if !last_sig.is_some_and(|p| p.is_alphanumeric() || p == '_' || p == '$') {
                last_word.clear();
            }
            last_word.push(c);
        } else if !c.is_whitespace() {
            last_word.clear();
        }
        if !c.is_whitespace() {
            last_sig = Some(c);
        }
        i += 1;
    }
    push(&mut out, start, chars.len(), start_line);
    out
}

fn regex_allowed(last: Option<char>, last_word: &str) -> bool {
    match last {
        None => true,
        Some(c) if c.is_alphanumeric() || c == '_' || c == '$' => {
            OPEN_KEYWORDS.contains(&last_word)
        }
        Some(')') | Some(']') | Some('}') | Some('"') | Some('\'') | Some('`') => false,
        Some(_) => true,
    }
}

fn should_split_at_newline(chars: &[char], from: usize, last_sig: Option<char>, last_word: &str) -> bool {
    let Some(last) = last_sig else {
        return false; // nothing accumulated yet
    };
    if CONTINUE_AFTER.contains(&last) {
        return false;
    }
    if (last.is_alphanumeric() || last == '_') && OPEN_KEYWORDS.contains(&last_word) {
        return false;
    }
    // `if (x)` / `for (…)` / `while (…)` followed by a body on the next line.
    let _ = last_word;
    // Find next significant char, skipping whitespace and comments.
    let mut j = from;
    loop {
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        if j + 1 < chars.len() && chars[j] == '/' && chars[j + 1] == '/' {
            while j < chars.len() && chars[j] != '\n' {
                j += 1;
            }
            continue;
        }
        if j + 1 < chars.len() && chars[j] == '/' && chars[j + 1] == '*' {
            j += 2;
            while j + 1 < chars.len() && !(chars[j] == '*' && chars[j + 1] == '/') {
                j += 1;
            }
            j += 2;
            continue;
        }
        break;
    }
    let Some(&next) = chars.get(j) else {
        return true;
    };
    if CONTINUE_BEFORE.contains(&next) {
        return false;
    }
    if next == '-' && chars.get(j + 1) != Some(&'-') {
        return false;
    }
    let word: String = chars[j..]
        .iter()
        .take_while(|c| c.is_alphanumeric() || **c == '_')
        .collect();
    !CONTINUE_KEYWORDS.contains(&word.as_str())
}

/// Returns true when the code fails to parse only because it is incomplete,
/// detected by the caller through the JS engine. Used to merge chunks that
/// the heuristic split too eagerly (e.g. `if (x)\n doIt()`).
pub fn merge_with_next(statements: &mut Vec<Statement>, index: usize) -> bool {
    if index + 1 >= statements.len() {
        return false;
    }
    let next = statements.remove(index + 1);
    let cur = &mut statements[index];
    cur.text.push('\n');
    cur.text.push_str(&next.text);
    true
}

/// Recognises shell commands that are not JavaScript (`use x`, `show dbs`,
/// `it`) and rewrites them as JS calls.
pub fn rewrite_shell_command(stmt: &str) -> Option<String> {
    let s = stmt.trim();
    let mut parts = s.split_whitespace();
    let first = parts.next()?;
    let rest: Vec<&str> = parts.collect();
    let quote = |x: &str| serde_json::to_string(x).unwrap_or_default();
    match (first, rest.as_slice()) {
        ("use", [name]) => Some(format!("__use({})", quote(name))),
        ("show", [what]) => Some(format!("__show({})", quote(what))),
        ("show", [what, arg]) => Some(format!("__show({}, {})", quote(what), quote(arg))),
        ("it", []) => Some("__it()".into()),
        ("help", []) => Some("help()".into()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(src: &str) -> Vec<String> {
        split_statements(src).into_iter().map(|s| s.text).collect()
    }

    #[test]
    fn simple_lines() {
        assert_eq!(texts("db.a.find()\ndb.b.find()"), vec!["db.a.find()", "db.b.find()"]);
        assert_eq!(texts("a = 1; b = 2;"), vec!["a = 1", "b = 2"]);
    }

    #[test]
    fn chained_calls() {
        assert_eq!(
            texts("db.getCollection('x')\n  .find({a: 1})\n  .sort({b: -1})\n\ndb.y.count()"),
            vec!["db.getCollection('x')\n  .find({a: 1})\n  .sort({b: -1})", "db.y.count()"]
        );
    }

    #[test]
    fn multi_line_objects() {
        let src = "db.c.insertOne({\n  a: 1,\n  b: 'x;y'\n})\nprint(1)";
        assert_eq!(texts(src), vec!["db.c.insertOne({\n  a: 1,\n  b: 'x;y'\n})", "print(1)"]);
    }

    #[test]
    fn comments_and_regex() {
        let src = "// comment; here\ndb.c.find({n: /a;b/i}) /* x;\n y */\nvar z = 4 / 2";
        // A leading comment stays attached to the statement that follows it.
        assert_eq!(texts(src), vec!["// comment; here\ndb.c.find({n: /a;b/i}) /* x;\n y */", "var z = 4 / 2"]);
    }

    #[test]
    fn functions_and_else() {
        let src = "function f(x) {\n  return x * 2;\n}\nif (a) {\n b()\n}\nelse {\n c()\n}";
        assert_eq!(texts(src), vec!["function f(x) {\n  return x * 2;\n}", "if (a) {\n b()\n}\nelse {\n c()\n}"]);
    }

    #[test]
    fn templates() {
        assert_eq!(texts("print(`a\n${1 + 2}\nb`)\nx"), vec!["print(`a\n${1 + 2}\nb`)", "x"]);
    }

    #[test]
    fn lines_reported() {
        let s = split_statements("\n\ndb.a.find()\n\nx");
        assert_eq!(s[0].line, 2);
        assert_eq!(s[1].line, 4);
    }

    #[test]
    fn shell_commands() {
        assert_eq!(rewrite_shell_command("use admin").unwrap(), "__use(\"admin\")");
        assert_eq!(rewrite_shell_command("show dbs").unwrap(), "__show(\"dbs\")");
        assert_eq!(rewrite_shell_command("show log global").unwrap(), "__show(\"log\", \"global\")");
        assert!(rewrite_shell_command("db.use.find()").is_none());
    }

    #[test]
    fn semicolons_inside_strings_and_regex() {
        assert_eq!(texts("print('a;b'); print(\"c;d\")"), vec!["print('a;b')", "print(\"c;d\")"]);
        assert_eq!(texts("var r = /;/g; r"), vec!["var r = /;/g", "r"]);
        assert_eq!(texts("x = 6 / 3; y = x / 2"), vec!["x = 6 / 3", "y = x / 2"]);
    }

    #[test]
    fn continuation_operators() {
        assert_eq!(texts("var a = 1 +\n  2\nb"), vec!["var a = 1 +\n  2", "b"]);
        assert_eq!(texts("var a = cond\n  ? 1\n  : 2"), vec!["var a = cond\n  ? 1\n  : 2"]);
        assert_eq!(texts("return\nfoo"), vec!["return\nfoo"]);
        assert_eq!(texts("try {\n x()\n}\ncatch (e) {}\nfinally {}"), vec!["try {\n x()\n}\ncatch (e) {}\nfinally {}"]);
    }

    #[test]
    fn blank_and_comment_only() {
        assert!(texts("").is_empty());
        assert!(texts("   \n\n  ;;").is_empty());
        assert_eq!(texts("/* only */").len(), 1);
    }

    #[test]
    fn merge_helper() {
        let mut s = split_statements("if (a)\nb()");
        assert_eq!(s.len(), 2);
        assert!(merge_with_next(&mut s, 0));
        assert_eq!(s[0].text, "if (a)\nb()");
        assert!(!merge_with_next(&mut s, 0));
    }

    #[test]
    fn shell_command_edge_cases() {
        assert_eq!(rewrite_shell_command("  use   my-db  ").unwrap(), "__use(\"my-db\")");
        assert_eq!(rewrite_shell_command("it").unwrap(), "__it()");
        assert_eq!(rewrite_shell_command("help").unwrap(), "help()");
        assert!(rewrite_shell_command("use").is_none());
        assert!(rewrite_shell_command("show").is_none());
        assert!(rewrite_shell_command("use a b").is_none());
    }
}
