use super::script_runner::{Form, Source, split_words};
use super::*;
use std::path::Path;

pub(super) fn is_typer(e: &FileEntry) -> bool {
    let Body::Text(texts) = &e.body else { return false };
    let TextDiff { local, remote, .. } = texts.diff();
    let is_py = Path::new(&e.path).extension().and_then(|x| x.to_str()).is_some_and(|x| x.eq_ignore_ascii_case("py"));
    [local, remote].iter().any(|text| {
        let python_shebang = text.lines().next().is_some_and(|l| l.starts_with("#!") && l.contains("python"));
        (is_py || python_shebang) && (text.contains("import typer") || text.contains("from typer"))
    })
}

#[derive(Clone, PartialEq, Debug, Default)]
pub(super) struct Param {
    pub(super) name: String,
    pub(super) option: Option<String>,
    pub(super) off: Option<String>,
    pub(super) is_bool: bool,
    pub(super) many: bool,
    pub(super) required: bool,
    pub(super) help: Option<String>,
    pub(super) default: String,
    pub(super) value: String,
    pub(super) default_on: bool,
    pub(super) on: bool,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub(super) struct TyperCommand {
    pub(super) name: String,
    pub(super) help: Option<String>,
    pub(super) params: Vec<Param>,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub(super) struct TyperForm {
    pub(super) python: String,
    pub(super) global: Vec<Param>,
    pub(super) commands: Vec<TyperCommand>,
    pub(super) selected: usize,
    pub(super) named: bool,
    pub(super) extra: String,
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

struct Scan<'a> {
    chars: std::str::CharIndices<'a>,
    depth: usize,
    quote: Option<char>,
    escaped: bool,
}

impl Iterator for Scan<'_> {
    type Item = (usize, char, usize);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (i, c) = self.chars.next()?;
            if let Some(q) = self.quote {
                if self.escaped {
                    self.escaped = false;
                } else if c == '\\' {
                    self.escaped = true;
                } else if c == q {
                    self.quote = None;
                }
                continue;
            }
            let depth = self.depth;
            match c {
                '"' | '\'' => {
                    self.quote = Some(c);
                    continue;
                }
                '(' | '[' | '{' => self.depth += 1,
                ')' | ']' | '}' => self.depth = self.depth.saturating_sub(1),
                _ => {}
            }
            return Some((i, c, depth));
        }
    }
}

fn outside_strings(text: &str) -> Scan<'_> {
    Scan { chars: text.char_indices(), depth: 0, quote: None, escaped: false }
}

fn closing(text: &str) -> Option<usize> {
    outside_strings(text).into_iter().skip(1).find(|&(_, c, depth)| matches!(c, ')' | ']' | '}') && depth == 1).map(|(i, _, _)| i)
}

fn split_top(text: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    for (i, c, depth) in outside_strings(text) {
        if c == separator && depth == 0 {
            parts.push(&text[start..i]);
            start = i + c.len_utf8();
        }
    }
    parts.push(&text[start..]);
    parts.into_iter().filter(|p| !p.trim().is_empty()).collect()
}

fn first_top(text: &str, wanted: char) -> Option<usize> {
    let chars: Vec<_> = outside_strings(text).collect();
    chars.iter().enumerate().find(|(n, (_, c, depth))| {
        let neighbor = |m: Option<&(usize, char, usize)>| m.is_some_and(|(_, c, _)| matches!(c, '=' | '!' | '<' | '>'));
        *c == wanted && *depth == 0 && !neighbor(n.checked_sub(1).and_then(|m| chars.get(m))) && !neighbor(chars.get(n + 1))
    }).map(|(_, (i, _, _))| *i)
}

fn string_literal(expr: &str) -> Option<String> {
    let expr = expr.trim();
    let expr = expr.strip_prefix(['r', 'R', 'f', 'F']).filter(|e| e.starts_with(['"', '\''])).unwrap_or(expr);
    let quote = ["\"\"\"", "'''", "\"", "'"].into_iter().find(|q| expr.starts_with(q) && expr.len() >= 2 * q.len() && expr.ends_with(q))?;
    Some(expr[quote.len()..expr.len() - quote.len()].to_string())
}

fn literal(expr: &str) -> Option<String> {
    let expr = expr.trim();
    if let Some(text) = string_literal(expr) {
        return Some(text);
    }
    if expr.parse::<f64>().is_ok() {
        return Some(expr.to_string());
    }
    let inner = expr.strip_prefix(['[', '(']).and_then(|e| e.strip_suffix([']', ')']))?;
    let items: Option<Vec<String>> = split_top(inner, ',').into_iter().map(literal).collect();
    Some(items?.iter().map(|i| if i.contains(char::is_whitespace) { format!("\"{i}\"") } else { i.clone() }).collect::<Vec<_>>().join(" "))
}

fn keyword<'a>(args: &[&'a str], name: &str) -> Option<&'a str> {
    args.iter().find_map(|a| {
        let (key, value) = a.split_at(first_top(a, '=')?);
        (key.trim() == name).then(|| value[1..].trim())
    })
}

fn positional<'a>(args: &[&'a str]) -> Vec<&'a str> {
    args.iter().filter(|a| first_top(a, '=').is_none()).map(|a| a.trim()).collect()
}

fn call_args<'a>(text: &'a str, callee: &str) -> Option<Vec<&'a str>> {
    let mut from = 0;
    while let Some(at) = text[from..].find(callee).map(|i| i + from) {
        from = at + callee.len();
        let starts_name = !text[..at].chars().next_back().is_some_and(is_name_char);
        if starts_name && text[from..].starts_with('(') {
            let close = from + closing(&text[from..])?;
            return Some(split_top(&text[from + 1..close], ','));
        }
    }
    None
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Argument,
    Option,
}

fn typer_call<'a>(text: &'a str) -> Option<(Kind, Vec<&'a str>)> {
    call_args(text, "Argument").map(|a| (Kind::Argument, a)).or_else(|| call_args(text, "Option").map(|a| (Kind::Option, a)))
}

fn base_type(annotation: &str) -> String {
    let annotation = annotation.trim();
    let inner = annotation.strip_prefix("Annotated[").or_else(|| annotation.strip_prefix("typing.Annotated[")).and_then(|a| split_top(a, ',').first().copied());
    let t = inner.unwrap_or(annotation).trim();
    let t = t.strip_prefix("Optional[").and_then(|t| t.strip_suffix(']')).unwrap_or(t);
    t.split('|').map(str::trim).find(|p| *p != "None").unwrap_or(t).to_string()
}

fn parameter(text: &str) -> Option<Param> {
    let text = text.trim();
    if text.is_empty() || text.starts_with('*') || text == "/" {
        return None;
    }
    let (head, default) = match first_top(text, '=') {
        Some(at) => (&text[..at], Some(text[at + 1..].trim())),
        None => (text, None),
    };
    let (name, annotation) = head.split_once(':').map_or((head.trim(), ""), |(n, a)| (n.trim(), a.trim()));
    if name.is_empty() || !name.chars().all(is_name_char) || name == "self" || annotation.contains("Context") {
        return None;
    }
    let in_annotation = typer_call(annotation);
    let in_default = default.and_then(typer_call);
    let (kind, call, default_expr) = match (in_annotation, in_default) {
        (Some((kind, args)), _) => (Some(kind), args, default.map(str::to_string)),
        (None, Some((kind, args))) => {
            let first = positional(&args).first().map(|d| d.to_string());
            (Some(kind), args[positional(&args).first().map_or(0, |_| 1)..].to_vec(), keyword(&args, "default").map(str::to_string).or(first))
        }
        (None, None) => (None, Vec::new(), default.map(str::to_string)),
    };
    let decls: Vec<String> = positional(&call).iter().filter_map(|a| string_literal(a)).filter(|d| d.trim_start().starts_with('-')).collect();
    let has_default = default_expr.as_deref().is_some_and(|d| d != "...");
    let kind = kind.unwrap_or(if has_default { Kind::Option } else { Kind::Argument });
    let t = base_type(annotation);
    let is_bool = t == "bool" && kind == Kind::Option;
    let lower = t.to_ascii_lowercase();
    let many = ["list[", "tuple[", "set[", "sequence[", "typing.list["].iter().any(|p| lower.starts_with(p));
    let (option, off) = match kind {
        Kind::Argument => (None, None),
        Kind::Option => {
            let flag = name.replace('_', "-");
            let decl = decls.iter().find(|d| d.starts_with("--")).or(decls.first());
            match decl.map(|d| d.split_once('/').map_or((d.trim().to_string(), None), |(on, off)| (on.trim().to_string(), Some(off.trim().to_string())))) {
                Some((on, off)) => (Some(on), off.filter(|o| !o.is_empty())),
                None => (Some(format!("--{flag}")), is_bool.then(|| format!("--no-{flag}"))),
            }
        }
    };
    let default_text = if has_default { default_expr.as_deref().and_then(literal).unwrap_or_default() } else { String::new() };
    let default_on = has_default && default_expr.as_deref().map(str::trim) == Some("True");
    Some(Param {
        name: name.to_string(),
        option,
        off,
        is_bool,
        many,
        required: !has_default,
        help: keyword(&call, "help").and_then(string_literal),
        value: default_text.clone(),
        default: default_text,
        default_on,
        on: default_on,
    })
}

fn docstring(body: &str) -> Option<String> {
    let rest = body[body.find(':')? + 1..].trim_start();
    let rest = rest.strip_prefix(['r', 'R']).unwrap_or(rest);
    let quote = ["\"\"\"", "'''", "\"", "'"].into_iter().find(|q| rest.starts_with(q))?;
    let inner = &rest[quote.len()..];
    inner[..inner.find(quote)?].lines().map(str::trim).find(|l| !l.is_empty()).map(str::to_string)
}

struct Def {
    decorators: Vec<String>,
    name: String,
    params: Vec<Param>,
    help: Option<String>,
}

fn definitions(script: &str) -> Vec<Def> {
    let mut defs = Vec::new();
    let mut decorators = Vec::new();
    let mut at = 0;
    while at < script.len() {
        let line_end = script[at..].find('\n').map_or(script.len(), |i| at + i + 1);
        let line = &script[at..line_end];
        let trimmed = line.trim_start();
        let start = at + line.len() - trimmed.len();
        at = line_end;
        if trimmed.starts_with('@') {
            let decorator = &script[start + 1..];
            let name_end = decorator.find(|c: char| !is_name_char(c) && c != '.').unwrap_or(decorator.len());
            let end = if decorator[name_end..].starts_with('(') { closing(&decorator[name_end..]).map_or(name_end, |close| name_end + close + 1) } else { name_end };
            decorators.push(decorator[..end].to_string());
            let decorator_end = start + 1 + end;
            at = script[decorator_end..].find('\n').map_or(script.len(), |i| decorator_end + i + 1);
        } else if let Some(def) = trimmed.strip_prefix("async ").unwrap_or(trimmed).strip_prefix("def ") {
            let def_at = start + trimmed.len() - def.len();
            let Some(open) = script[def_at..].find('(').map(|i| def_at + i) else { break };
            let Some(close) = closing(&script[open..]).map(|i| open + i) else { break };
            defs.push(Def {
                decorators: std::mem::take(&mut decorators),
                name: script[def_at..open].trim().to_string(),
                params: split_top(&script[open + 1..close], ',').into_iter().filter_map(parameter).collect(),
                help: docstring(&script[close..]),
            });
            at = script[close..].find('\n').map_or(script.len(), |i| close + i + 1);
        } else if !trimmed.is_empty() && !trimmed.starts_with('#') {
            decorators.clear();
        }
    }
    defs
}

const MAX_DEPTH: usize = 8;

enum Import {
    Module(String),
    Name(String, String),
}

struct Module {
    path: String,
    text: String,
    defs: Vec<Def>,
    imports: Vec<(String, Import)>,
    apps: Vec<(String, Option<String>)>,
}

impl Module {
    fn parse(path: &str, text: String) -> Module {
        Module { path: path.to_string(), defs: definitions(&text), imports: imports(&text), apps: typer_apps(&text), text }
    }

    fn import(&self, local: &str) -> Option<&Import> {
        self.imports.iter().rev().find(|(name, _)| name == local).map(|(_, import)| import)
    }

    fn is_package(&self) -> bool {
        self.path == "__init__.py" || self.path.ends_with("/__init__.py")
    }
}

fn is_dotted_name(text: &str) -> bool {
    !text.is_empty() && text.split('.').all(|part| !part.is_empty() && part.chars().all(is_name_char))
}

fn without_comment(line: &str) -> &str {
    line.find('#').map_or(line, |i| &line[..i])
}

fn imported_names(list: &str) -> Vec<(String, String)> {
    list.split(',')
        .filter_map(|item| {
            let words: Vec<&str> = item.split_whitespace().collect();
            match words.as_slice() {
                [name] if is_dotted_name(name) => Some((name.to_string(), name.to_string())),
                [name, "as", alias] if is_dotted_name(name) => Some((name.to_string(), alias.to_string())),
                _ => None,
            }
        })
        .collect()
}

fn imports(text: &str) -> Vec<(String, Import)> {
    let mut found = Vec::new();
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let start = at + line.len() - trimmed.len();
        at += line.len();
        if let Some(rest) = trimmed.strip_prefix("import ") {
            for (name, alias) in imported_names(without_comment(rest)) {
                if name == alias {
                    let top = name.split('.').next().unwrap_or(&name).to_string();
                    found.push((top.clone(), Import::Module(top)));
                } else {
                    found.push((alias, Import::Module(name)));
                }
            }
        } else if let Some(rest) = trimmed.strip_prefix("from ")
            && let Some((module, names)) = rest.split_once(" import ")
        {
            let module = module.trim();
            let names = names.trim_start();
            let list = match names.strip_prefix('(') {
                Some(_) => {
                    let open = start + (trimmed.len() - names.len());
                    closing(&text[open..]).map_or("", |close| &text[open + 1..open + close])
                }
                None => without_comment(names),
            };
            let list: String = list.lines().map(without_comment).collect::<Vec<_>>().join(",");
            for (name, alias) in imported_names(&list) {
                found.push((alias, Import::Name(module.to_string(), name)));
            }
        }
    }
    found
}
fn typer_apps(text: &str) -> Vec<(String, Option<String>)> {
    let mut found = Vec::new();
    for line in text.lines() {
        let Some(at) = first_top(line, '=') else { continue };
        let name = line[..at].split(':').next().unwrap_or_default().trim();
        let value = line[at + 1..].trim_start();
        let Some(call) = value.strip_prefix("typer.Typer").or_else(|| value.strip_prefix("Typer")).filter(|c| c.starts_with('(')) else { continue };
        if name.is_empty() || !name.chars().all(is_name_char) {
            continue;
        }
        found.push((name.to_string(), keyword(&decorator_args(call), "name").and_then(string_literal)));
    }
    found
}

fn calls_at_line_start<'a>(text: &'a str, callee: &str) -> Vec<(Vec<&'a str>, &'a str)> {
    let mut found = Vec::new();
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let open = at + line.len() - trimmed.len() + callee.len();
        at += line.len();
        if trimmed.strip_prefix(callee).is_some_and(|rest| rest.starts_with('('))
            && let Some(close) = closing(&text[open..]).map(|close| open + close)
        {
            found.push((split_top(&text[open + 1..close], ','), &text[close + 1..]));
        }
    }
    found
}

fn chained_target(rest: &str) -> Option<&str> {
    rest.starts_with('(').then(|| closing(rest)).flatten().map(|close| rest[1..close].trim())
}

fn decorator_args(call: &str) -> Vec<&str> {
    call.find('(').and_then(|open| closing(&call[open..]).map(|close| split_top(&call[open + 1..open + close], ','))).unwrap_or_default()
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn join_path(dir: &str, rel: &str) -> String {
    if dir.is_empty() { rel.to_string() } else { format!("{dir}/{rel}") }
}

fn join_module(module: &str, name: &str) -> String {
    if module.ends_with('.') { format!("{module}{name}") } else { format!("{module}.{name}") }
}

struct Files<'a> {
    read: &'a dyn Fn(&str) -> Option<String>,
    loaded: std::cell::RefCell<std::collections::HashMap<String, Option<std::rc::Rc<Module>>>>,
}

type Found<T> = Option<(std::rc::Rc<Module>, T)>;

impl Files<'_> {
    fn module_bases(&self, from: &Module, dotted: &str) -> Option<(Vec<String>, String)> {
        let rest = dotted.trim_start_matches('.');
        let dots = dotted.len() - rest.len();
        if !rest.is_empty() && !is_dotted_name(rest) {
            return None;
        }
        let rel = rest.replace('.', "/");
        if dots > 0 {
            let mut base = parent_dir(&from.path);
            for _ in 1..dots {
                if base.is_empty() {
                    return None;
                }
                base = parent_dir(base);
            }
            return Some((vec![base.to_string()], rel));
        }
        let mut bases = Vec::new();
        let mut dir = parent_dir(&from.path);
        loop {
            bases.push(dir.to_string());
            bases.push(join_path(dir, "src"));
            if dir.is_empty() {
                break;
            }
            dir = parent_dir(dir);
        }
        Some((bases, rel))
    }

    fn load(&self, from: &Module, dotted: &str) -> Option<std::rc::Rc<Module>> {
        let (bases, rel) = self.module_bases(from, dotted)?;
        bases.iter().find_map(|base| {
            let package = join_path(base, &join_path(&rel, "__init__.py"));
            let candidates = if rel.is_empty() { vec![package] } else { vec![join_path(base, &format!("{rel}.py")), package] };
            candidates.into_iter().find_map(|path| self.file(&path))
        })
    }

    fn file(&self, path: &str) -> Option<std::rc::Rc<Module>> {
        if let Some(known) = self.loaded.borrow().get(path) {
            return known.clone();
        }
        let module = (self.read)(path).map(|text| std::rc::Rc::new(Module::parse(path, text)));
        self.loaded.borrow_mut().insert(path.to_string(), module.clone());
        module
    }

    fn module_named(&self, module: &Module, local: &str) -> Option<std::rc::Rc<Module>> {
        match module.import(local) {
            Some(Import::Module(dotted)) => self.load(module, dotted),
            Some(Import::Name(from, name)) => self.load(module, &join_module(from, name)),
            None if module.is_package() => self.load(module, &format!(".{local}")),
            None => None,
        }
    }

    fn find<T>(&self, module: &std::rc::Rc<Module>, expr: &str, depth: usize, pick: &dyn Fn(&Module, &str) -> Option<T>) -> Found<T> {
        let expr = expr.trim();
        if depth > MAX_DEPTH {
            return None;
        }
        match expr.split_once('.') {
            None => {
                if let Some(found) = pick(module, expr) {
                    return Some((module.clone(), found));
                }
                let Some(Import::Name(from, name)) = module.import(expr) else { return None };
                let source = self.load(module, from)?;
                self.find(&source, name, depth + 1, pick)
            }
            Some((head, rest)) => {
                let target = self.module_named(module, head)?;
                self.find(&target, rest, depth + 1, pick)
            }
        }
    }
}

fn app_named(module: &Module, name: &str) -> Option<String> {
    module.apps.iter().any(|(app, _)| app == name).then(|| name.to_string())
}

fn def_named(module: &Module, name: &str) -> Option<usize> {
    module.defs.iter().position(|d| d.name == name)
}

struct Walk<'a> {
    files: Files<'a>,
    form: TyperForm,
    has_callback: bool,
    has_groups: bool,
}

impl Walk<'_> {
    fn add_command(&mut self, prefix: &str, args: &[&str], def: &Def) {
        let own = positional(args).first().and_then(|a| string_literal(a)).or_else(|| keyword(args, "name").and_then(string_literal)).unwrap_or_else(|| def.name.replace('_', "-"));
        let help = keyword(args, "help").and_then(string_literal).or_else(|| def.help.clone());
        self.form.commands.push(TyperCommand { name: join_command(prefix, &own), help, params: def.params.clone() });
    }

    fn set_callback(&mut self, def: &Def) {
        self.has_callback = true;
        self.form.global = def.params.clone();
    }

    fn app(&mut self, module: &std::rc::Rc<Module>, app: &str, prefix: &str, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        for def in &module.defs {
            for decorator in &def.decorators {
                let Some(call) = decorator.strip_prefix(app).and_then(|d| d.strip_prefix('.')) else { continue };
                if call.starts_with("command") {
                    self.add_command(prefix, &decorator_args(call), def);
                } else if call.starts_with("callback") && depth == 0 {
                    self.set_callback(def);
                }
            }
        }
        for (args, rest) in calls_at_line_start(&module.text, &format!("{app}.command")) {
            if let Some((source, index)) = chained_target(rest).and_then(|target| self.files.find(module, target, 0, &def_named)) {
                self.add_command(prefix, &args, &source.defs[index]);
            }
        }
        if depth == 0 {
            for (_, rest) in calls_at_line_start(&module.text, &format!("{app}.callback")) {
                if let Some((source, index)) = chained_target(rest).and_then(|target| self.files.find(module, target, 0, &def_named)) {
                    self.set_callback(&source.defs[index]);
                }
            }
        }
        for (args, _) in calls_at_line_start(&module.text, &format!("{app}.add_typer")) {
            self.has_groups = true;
            let Some((source, sub)) = positional(&args).first().and_then(|target| self.files.find(module, target, 0, &app_named)) else { continue };
            let own_name = source.apps.iter().find(|(name, _)| *name == sub).and_then(|(_, name)| name.clone());
            let name = keyword(&args, "name").and_then(string_literal).or(own_name);
            let prefix = name.map_or(prefix.to_string(), |name| join_command(prefix, &name));
            self.app(&source, &sub, &prefix, depth + 1);
        }
    }
}

fn join_command(prefix: &str, name: &str) -> String {
    if prefix.is_empty() { name.to_string() } else { format!("{prefix} {name}") }
}

fn root_apps(module: &Module) -> Vec<String> {
    let added: Vec<String> = module
        .apps
        .iter()
        .flat_map(|(app, _)| calls_at_line_start(&module.text, &format!("{app}.add_typer")))
        .filter_map(|(args, _)| positional(&args).first().map(|a| a.to_string()))
        .collect();
    let roots: Vec<&String> = module.apps.iter().map(|(app, _)| app).filter(|app| !added.contains(app)).collect();
    let invoked = roots.iter().find(|app| module.text.lines().any(|l| l.trim() == format!("{app}()")));
    if let Some(app) = invoked.or_else(|| roots.iter().find(|app| app.as_str() == "app")).or(roots.first()) {
        return vec![app.to_string()];
    }
    let mut decorated = Vec::new();
    for def in &module.defs {
        for decorator in &def.decorators {
            if let Some((app, call)) = decorator.split_once('.')
                && call.starts_with("command")
                && !decorated.iter().any(|d| d == app)
            {
                decorated.push(app.to_string());
            }
        }
    }
    decorated
}

pub(super) fn parse_typer(source: &Source) -> TyperForm {
    let files = Files { read: source.read, loaded: Default::default() };
    let script = std::rc::Rc::new(Module::parse(source.path, source.text.to_string()));
    let mut walk = Walk { files, form: TyperForm { python: "python3".to_string(), ..TyperForm::default() }, has_callback: false, has_groups: false };
    for app in root_apps(&script) {
        walk.app(&script, &app, "", 0);
    }
    if walk.form.commands.is_empty()
        && let Some(target) = call_args(source.text, "typer.run").and_then(|args| positional(&args).first().map(|a| a.to_string()))
        && let Some((module, index)) = walk.files.find(&script, &target, 0, &def_named)
    {
        let def = &module.defs[index];
        walk.form.commands.push(TyperCommand { name: String::new(), help: def.help.clone(), params: def.params.clone() });
    }
    let mut form = walk.form;
    form.named = form.commands.iter().any(|c| !c.name.is_empty()) && (form.commands.len() > 1 || walk.has_callback || walk.has_groups);
    form
}

fn param_arguments(params: &[Param]) -> Vec<String> {
    let mut args = Vec::new();
    for p in params {
        let Some(option) = &p.option else { continue };
        if p.is_bool {
            match (p.on, p.default_on) {
                (true, false) => args.push(option.clone()),
                (false, true) => args.extend(p.off.clone()),
                _ => {}
            }
        } else if !p.value.is_empty() && (p.value != p.default || p.required) {
            let values = if p.many { split_words(&p.value) } else { vec![p.value.clone()] };
            for value in values {
                args.push(option.clone());
                args.push(value);
            }
        }
    }
    let positionals: Vec<&Param> = params.iter().filter(|p| p.option.is_none()).collect();
    let filled = positionals.iter().rposition(|p| !p.value.is_empty()).map_or(0, |i| i + 1);
    for p in &positionals[..filled] {
        if p.many {
            args.extend(split_words(&p.value));
        } else {
            args.push(p.value.clone());
        }
    }
    args
}

impl Form for TyperForm {
    fn program(&self) -> Vec<String> {
        split_words(&self.python)
    }

    fn arguments(&self) -> Vec<String> {
        let mut args = param_arguments(&self.global);
        if let Some(command) = self.commands.get(self.selected) {
            if self.named {
                args.extend(command.name.split(' ').map(str::to_string));
            }
            args.extend(param_arguments(&command.params));
        }
        args.extend(split_words(&self.extra));
        args
    }

    fn environment(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        if self.commands.len() > 1 {
            let selected = self.commands.get(self.selected).map(|c| c.name.clone()).unwrap_or_default();
            egui::ComboBox::from_id_salt("typer-command").selected_text(RichText::new(selected).monospace()).show_ui(ui, |ui| {
                for (i, command) in self.commands.iter().enumerate() {
                    ui.selectable_value(&mut self.selected, i, RichText::new(&command.name).monospace());
                }
            });
        }
        if let Some(help) = self.commands.get(self.selected).and_then(|c| c.help.as_ref()) {
            ui.weak(help);
        }
        egui::Grid::new("typer-form").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            ui.label("python");
            ui.add(egui::TextEdit::singleline(&mut self.python).desired_width(220.0).hint_text("python3")).on_hover_text("The interpreter, e.g. .venv/bin/python or uv run python");
            ui.end_row();
            for param in &mut self.global {
                param_ui(ui, param);
            }
            if let Some(command) = self.commands.get_mut(self.selected) {
                for param in &mut command.params {
                    param_ui(ui, param);
                }
            }
            ui.label("extra arguments");
            ui.add(egui::TextEdit::singleline(&mut self.extra).desired_width(220.0).hint_text("--verbose \"a b\""));
            ui.end_row();
        });
    }
}

fn param_ui(ui: &mut egui::Ui, param: &mut Param) {
    let label = match &param.option {
        Some(option) => format!("{option}{}", if param.required { " *" } else { "" }),
        None => format!("{}{}", param.name, if param.required { " *" } else { "" }),
    };
    let label = RichText::new(label).monospace();
    let response = if param.is_bool {
        let response = ui.checkbox(&mut param.on, label);
        ui.label("");
        response
    } else {
        let response = ui.label(label);
        let hint = if param.many { "values separated by spaces" } else { "" };
        ui.add(egui::TextEdit::singleline(&mut param.value).desired_width(220.0).hint_text(hint));
        response
    };
    if let Some(help) = &param.help {
        response.on_hover_text(help);
    }
    ui.end_row();
}

#[cfg(test)]
mod tests {
    use super::super::script_runner::{Place, ScriptRunner};
    use super::*;

    const CLI: &str = r#"from typing import Annotated, List, Optional
import typer

app = typer.Typer()
users = typer.Typer()
app.add_typer(users, name="users")

@app.callback()
def main(verbose: bool = False):
    """Manage the site."""

@app.command()
def deploy(
    target: str,
    version: Annotated[str, typer.Argument(help="Version to ship")] = "latest",
    region: str = typer.Option("eu-west-1", "--region", "-r", help="Where"),
    dry_run: bool = typer.Option(False, "--dry-run/--for-real"),
    tags: Annotated[Optional[List[str]], typer.Option()] = None,
    cache: bool = True,
):
    """Deploy the site.

    Long description.
    """

@users.command("add")
def add_user(ctx: typer.Context, name: str, admin: Annotated[bool, typer.Option("--admin")] = False):
    ...

if __name__ == "__main__":
    app()
"#;

    fn parse(text: &str) -> TyperForm {
        parse_typer(&Source { text, path: "cli.py", read: &|_| None })
    }

    const MAIN: &str = r#"import typer

from wmm import crawl, model_metadata, telemetry
from .tools import tripo as tripo_tools

app = typer.Typer(help="Tooling.")

models_app = typer.Typer(help="3D models.")
models_app.add_typer(tripo_tools.app, name="tripo")
models_app.command("metadata")(model_metadata.metadata_command)

telemetry_app = typer.Typer()
telemetry_app.command("report")(telemetry.report_command)

app.add_typer(crawl.app, name="crawl")
app.add_typer(models_app, name="models")
app.add_typer(telemetry_app, name="telemetry")
"#;

    fn workspace_file(path: &str) -> Option<String> {
        let text = match path {
            "wmm/crawl.py" => "import typer\napp = typer.Typer()\n\n@app.command()\ndef run_all(depth: int = 2):\n    \"\"\"Crawl everything.\"\"\"\n",
            "wmm/model_metadata.py" => "def metadata_command(\n    model: str,\n    force: bool = False,\n):\n    pass\n",
            "wmm/telemetry.py" => "from .reports import report_command\n",
            "wmm/reports.py" => "def report_command(day: str = \"today\"):\n    pass\n",
            "wmm/tools/tripo.py" => "from typer import Typer\napp = Typer()\n\n@app.command(\"generate\")\ndef generate(prompt: str):\n    pass\n",
            _ => return None,
        };
        Some(text.to_string())
    }

    #[test]
    fn follows_sub_apps_and_commands_into_other_modules() {
        let form = parse_typer(&Source { text: MAIN, path: "wmm/cli.py", read: &workspace_file });
        assert!(form.named);
        let names: Vec<_> = form.commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["crawl run-all", "models metadata", "models tripo generate", "telemetry report"]);
        assert_eq!(form.commands[0].help.as_deref(), Some("Crawl everything."));
        let params: Vec<_> = form.commands.iter().map(|c| c.params.iter().map(|p| p.name.as_str()).collect::<Vec<_>>()).collect();
        assert_eq!(params, [vec!["depth"], vec!["model", "force"], vec!["prompt"], vec!["day"]]);
    }

    #[test]
    fn form_from_a_typer_cli() {
        let form = parse(CLI);
        assert!(form.named);
        assert_eq!(form.commands.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["deploy", "users add"]);
        assert_eq!(form.global.iter().map(|p| (p.option.clone(), p.off.clone())).collect::<Vec<_>>(), [(Some("--verbose".into()), Some("--no-verbose".into()))]);
        let deploy = &form.commands[0];
        assert_eq!(deploy.help.as_deref(), Some("Deploy the site."));
        let summary: Vec<_> = deploy.params.iter().map(|p| (p.name.as_str(), p.option.as_deref(), p.off.as_deref(), p.is_bool, p.many, p.required, p.default.as_str())).collect();
        assert_eq!(
            summary,
            [
                ("target", None, None, false, false, true, ""),
                ("version", None, None, false, false, false, "latest"),
                ("region", Some("--region"), None, false, false, false, "eu-west-1"),
                ("dry_run", Some("--dry-run"), Some("--for-real"), true, false, false, ""),
                ("tags", Some("--tags"), None, false, true, false, ""),
                ("cache", Some("--cache"), Some("--no-cache"), true, false, false, ""),
            ]
        );
        assert_eq!(deploy.params[1].help.as_deref(), Some("Version to ship"));
        assert!(deploy.params[5].default_on);
        assert_eq!(form.commands[1].params.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["name", "admin"]);
    }

    #[test]
    fn arguments_of_the_selected_command() {
        let mut form = parse(CLI);
        form.global[0].on = true;
        let deploy = &mut form.commands[0];
        deploy.params[0].value = "site".into();
        deploy.params[2].value = "us-east-2".into();
        deploy.params[3].on = true;
        deploy.params[4].value = "a 'b c'".into();
        deploy.params[5].on = false;
        assert_eq!(form.arguments(), ["--verbose", "deploy", "--region", "us-east-2", "--dry-run", "--tags", "a", "--tags", "b c", "--no-cache", "site", "latest"]);
        form.selected = 1;
        form.global[0].on = false;
        form.commands[1].params[0].value = "ana".into();
        form.commands[1].params[1].on = true;
        form.extra = "--help".into();
        assert_eq!(form.arguments(), ["users", "add", "--admin", "ana", "--help"]);
    }

    #[test]
    fn single_command_apps_need_no_command_name() {
        let script = "import typer\n\ndef main(name: str, count: int = 1):\n    print(name)\n\nif __name__ == \"__main__\":\n    typer.run(main)\n";
        let mut form = parse(script);
        assert!(!form.named);
        form.commands[0].params[0].value = "ana".into();
        form.commands[0].params[1].value = "3".into();
        assert_eq!(form.arguments(), ["--count", "3", "ana"]);
        let app = "import typer\napp = typer.Typer()\n\n@app.command()\ndef hello(name: str):\n    pass\n";
        assert!(!parse(app).named);
    }

    #[test]
    fn runs_the_local_cli_with_python() {
        let script = "import sys\nclass App:\n    def command(self):\n        return lambda f: f\napp = App()\n# import typer\n@app.command()\ndef greet(name: str, loud: bool = False):\n    pass\nprint(' '.join(sys.argv[1:]))\n";
        let root = std::env::temp_dir().join(format!("agent-diff-typer-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("cli.py"), script).unwrap();
        let entry = FileEntry {
            path: "cli.py".into(),
            rename: None,
            origin: Origin::Synced,
            local_change: None,
            remote_change: None,
            local_exists: true,
            remote_exists: true,
            body: Body::Text(Texts::new(script, script)),
            version: 1,
        };
        assert!(is_typer(&entry));
        let local_root = root.to_str().unwrap().to_string();
        let mirror = root.join("mirror");
        let place = FilePlace { local_root: &local_root, mirror: &mirror, remote: &local_root };
        let mut runner = ScriptRunner::load(&entry, &place, parse_typer);
        runner.form.commands[0].params[0].value = "ana".into();
        runner.form.commands[0].params[1].on = true;
        runner.start(Place::Local, &egui::Context::default());
        for _ in 0..300 {
            runner.poll();
            if runner.run.as_ref().is_some_and(|r| r.finished.is_some()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(runner.run.as_ref().unwrap().finished.unwrap().0, Some(0));
        assert_eq!(runner.output_text(), "--loud ana");
        let _ = std::fs::remove_dir_all(root);
    }
}
