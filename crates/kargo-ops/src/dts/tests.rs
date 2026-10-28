use std::path::Path;

use super::*;

fn write(root: &Path, file: &str, text: &str) {
    let path = root.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// An express-like package: the runtime package without types, `@types/express` for a CommonJS
/// module (`export =` a function and a namespace), and a types-only package it builds on.
fn express(node_modules: &Path) {
    write(node_modules, "express/package.json", r#"{"name":"express","version":"5.1.0","main":"index.js"}"#);
    write(node_modules, "express/index.js", "module.exports = require('./lib/express');");
    write(node_modules, "@types/express/package.json", r#"{"name":"@types/express","types":"index.d.ts"}"#);
    write(
        node_modules,
        "@types/express/index.d.ts",
        r#"
        /// <reference types="express-serve-static-core" />
        import * as core from "express-serve-static-core";
        import bodyParser = require("body-parser");

        /** Creates an Express application. */
        declare function e(): core.Express;

        declare namespace e {
            var json: typeof bodyParser.json;
            export function Router(options?: RouterOptions): core.Router;
            interface RouterOptions {
                caseSensitive?: boolean | undefined;
            }
            interface Request<P = core.ParamsDictionary> extends core.Request<P> {}
            interface Express extends core.Express {}
        }

        export = e;
        "#,
    );
    write(node_modules, "@types/body-parser/package.json", r#"{"name":"@types/body-parser","types":"index.d.ts"}"#);
    write(
        node_modules,
        "@types/body-parser/index.d.ts",
        r#"
        import { NextHandleFunction } from "connect";
        declare namespace bodyParser {
            interface OptionsJson { strict?: boolean | undefined; }
            function json(options?: OptionsJson): NextHandleFunction;
        }
        export = bodyParser;
        "#,
    );
    write(
        node_modules,
        "@types/express-serve-static-core/index.d.ts",
        r#"
        import * as http from "http";
        declare global { namespace Express { interface Request {} } }
        export interface NextFunction {
            (err?: any): void;
            (deferToNext: "router"): void;
        }
        export interface ParamsDictionary { [key: string]: string; }
        export interface RequestHandler<P = ParamsDictionary> {
            (req: Request<P>, res: Response, next: NextFunction): unknown;
        }
        export type ErrorRequestHandler = (err: any, req: Request, res: Response, next: NextFunction) => unknown;
        export type PathParams = string | RegExp | Array<string | RegExp>;
        export type RequestHandlerParams = RequestHandler | ErrorRequestHandler | Array<RequestHandler>;
        export interface IRouterMatcher<T> {
            <Route extends string | RegExp>(path: Route, ...handlers: Array<RequestHandler>): T;
            (path: PathParams, ...handlers: Array<RequestHandlerParams>): T;
        }
        export interface IRouterHandler<T> {
            (...handlers: Array<RequestHandlerParams>): T;
        }
        export interface IRouter extends RequestHandler {
            get: IRouterMatcher<this>;
            "m-search": IRouterMatcher<this>;
            use: IRouterHandler<this> & IRouterMatcher<this>;
            delete: IRouterMatcher<this>;
        }
        export interface Router extends IRouter {}
        export interface Request<P = ParamsDictionary> extends http.IncomingMessage, Express.Request {
            /** The path part of the URL. */
            readonly path: string;
            params: P;
            get(name: string): string | undefined;
            accepts(...type: string[]): string | false;
        }
        export interface Response {
            status(code: number): this;
            send: Send<this>;
            locals: Record<string, any>;
        }
        export type Send<T = Response> = (body?: any) => T;
        export interface Application extends IRouter {
            (req: Request, res: Response): any;
            get: ((name: string) => any) & IRouterMatcher<this>;
            listen(port: number, callback?: (error?: Error) => void): http.Server;
            listen(path: string, callback?: (error?: Error) => void): http.Server;
            in: boolean;
            toString(): string;
        }
        export interface Express extends Application {
            request: Request;
        }
        "#,
    );
}

fn generated(packages: &[&str], setup: impl Fn(&Path)) -> (tempfile::TempDir, Generated) {
    let dir = tempfile::tempdir().unwrap();
    let node_modules = dir.path().join("node_modules");
    setup(&node_modules);
    let packages: Vec<String> = packages.iter().map(|p| p.to_string()).collect();
    let out = generate(&packages, std::slice::from_ref(&node_modules), &dir.path().join("out")).unwrap();
    (dir, out)
}

/// The members of the declaration that starts with `start` in `text`, each line with its `\n`.
fn body(text: &str, start: &str) -> String {
    let rest = text.split(start).nth(1).unwrap_or_else(|| panic!("no `{start}` in:\n{text}"));
    format!("{}\n", rest.split("\n}").next().unwrap())
}

fn read(dir: &tempfile::TempDir, file: &str) -> String {
    std::fs::read_to_string(dir.path().join("out").join(file)).unwrap_or_else(|e| panic!("{file}: {e}"))
}

#[test]
fn express_from_its_types_package() {
    let (dir, out) = generated(&["express"], express);
    assert!(out.untyped.is_empty());
    let default = read(&dir, "express/Default.kt");
    assert!(default.contains("package express\n"), "{default}");
    assert!(
        default.contains(
            "/**\n * Creates an Express application.\n */\n@JsModule(\"express\")\nexternal fun express(): express_serve_static_core.Express"
        ),
        "{default}"
    );
    let exports = read(&dir, "express/Exports.kt");
    assert!(exports.contains("@file:JsModule(\"express\")\n\npackage express"), "{exports}");
    assert!(
        exports.contains("external fun Router(options: express.RouterOptions = definedExternally): express_serve_static_core.Router"),
        "{exports}"
    );
    // `typeof bodyParser.json` is that function.
    assert!(exports.contains("external fun json(options: body_parser.OptionsJson = definedExternally): JsAny"), "{exports}");

    let types = read(&dir, "express/Types.kt");
    assert!(types.contains("external interface RouterOptions : JsAny {\n    var caseSensitive: Boolean?\n}"), "{types}");
    assert!(types.contains("typealias Request = express_serve_static_core.Request"), "{types}");
    assert!(types.contains("typealias Express = express_serve_static_core.Express"), "{types}");

    let core = read(&dir, "express_serve_static_core/Types.kt");
    // Supertypes are copied in, `this` is the type itself, and a callable property is methods.
    let express = body(&core, "external interface Express : JsAny {");
    assert!(express.contains("    var request: express_serve_static_core.Request\n"), "{express}");
    assert!(express.contains("    fun get(name: String): JsAny?\n"), "{express}");
    let handler = "(express_serve_static_core.Request, express_serve_static_core.Response, (JsAny?) -> Unit) -> Unit";
    assert!(express.contains(&format!("    fun get(path: String, handlers: {handler}): express_serve_static_core.Express\n")), "{express}");
    assert!(
        express.contains(&format!("    fun get(path: String, vararg handlers: {handler}): express_serve_static_core.Express\n")),
        "{express}"
    );
    assert!(express.contains("    fun get(path: JsAny, vararg handlers: JsAny): express_serve_static_core.Express\n"), "{express}");
    // An error handler takes four parameters.
    assert!(express.contains("    fun use(handlers: (JsAny?, express_serve_static_core.Request,"), "{express}");
    assert!(express.contains("    fun delete(path: String"), "{express}");
    assert!(express.contains("    var `in`: Boolean\n"), "{express}");
    // An optional parameter of several types: an overload without it, and one for each type.
    assert!(
        express.contains("    fun listen(port: Int): JsAny\n    fun listen(port: Int, callback: (JsAny?) -> Unit): JsAny\n"),
        "{express}"
    );
    assert!(express.contains("    fun listen(port: Double, callback: (JsAny?) -> Unit): JsAny\n"), "{express}");
    assert!(!express.contains("= definedExternally"), "{express}");
    assert!(!express.contains("m-search") && !express.contains("toString"), "{express}");

    let request = body(&core, "external interface Request : JsAny {");
    assert!(request.contains("    /**\n     * The path part of the URL.\n     */\n    val path: String\n"), "{request}");
    assert!(request.contains("    var params: express_serve_static_core.ParamsDictionary\n"), "{request}");
    assert!(request.contains("    fun accepts(vararg type: String): JsAny\n"), "{request}");
    let response = body(&core, "external interface Response : JsAny {");
    assert!(response.contains("    fun status(code: Int): express_serve_static_core.Response\n"), "{response}");
    // `any` takes a Kotlin string too.
    assert!(response.contains("    fun send(): express_serve_static_core.Response\n"), "{response}");
    assert!(response.contains("    fun send(body: JsAny?): express_serve_static_core.Response\n"), "{response}");
    assert!(response.contains("    fun send(body: String): express_serve_static_core.Response\n"), "{response}");
    assert!(response.contains("    var locals: JsAny\n"), "{response}");
    // `req.params["id"]`, as an extension where `import express.*` finds it.
    assert!(
        types.contains(
            "operator fun express_serve_static_core.ParamsDictionary.get(key: String): String? = ParamsDictionaryIndexGet(this, key)"
        ),
        "{types}"
    );
    assert!(!core.contains("operator fun"), "{core}");
    // Only callable, so a function type rather than an interface.
    assert!(!core.contains("interface RequestHandler") && !core.contains("interface NextFunction"), "{core}");
}

#[test]
fn es_modules_classes_and_missing_types() {
    let (dir, out) = generated(&["@acme/greeter", "untyped", "@types/node"], |nm| {
        write(
            nm,
            "@acme/greeter/package.json",
            r#"{"name":"@acme/greeter","exports":{".":{"types":"./dist/index.d.ts","import":"./dist/index.js"}}}"#,
        );
        write(
            nm,
            "@acme/greeter/dist/index.d.ts",
            "export * from './greeter';\nexport { default as shout } from './shout';\nexport declare const version: string;\n",
        );
        write(
            nm,
            "@acme/greeter/dist/greeter.d.ts",
            r#"
            export interface Options { loud?: boolean }
            export declare class Greeter {
                constructor(name: string, options?: Options);
                greet(times?: number): string;
                static create(): Greeter;
                private secret;
                get name(): string;
            }
            export declare function greet(name: string | string[]): Promise<string>;
            "#,
        );
        write(nm, "@acme/greeter/dist/shout.d.ts", "export default function shout(text: string): string;\n");
        write(nm, "untyped/package.json", r#"{"name":"untyped","main":"index.js"}"#);
    });
    assert_eq!(out.untyped, ["untyped"]);
    let exports = read(&dir, "acme/greeter/Exports.kt");
    assert!(exports.contains("@file:JsModule(\"@acme/greeter\")\n\npackage acme.greeter"), "{exports}");
    assert!(exports.contains("external val version: String"), "{exports}");
    assert!(exports.contains("external fun greet(name: String): Promise<JsString>"), "{exports}");
    assert!(exports.contains("external fun greet(name: JsArray<JsString>): Promise<JsString>"), "{exports}");
    assert!(exports.contains("external fun shout(text: String): String"), "{exports}");
    assert!(
        exports.contains(
            "external class Greeter : JsAny {\n    constructor(name: String, options: acme.greeter.Options = definedExternally)\n"
        ),
        "{exports}"
    );
    assert!(
        exports.contains(
            "    val name: String\n    fun greet(): String\n    fun greet(times: Int): String\n    fun greet(times: Double): String\n"
        ),
        "{exports}"
    );
    assert!(!exports.contains("create") && !exports.contains("secret"), "{exports}");
    let types = read(&dir, "acme/greeter/Types.kt");
    assert!(types.contains("external interface Options : JsAny {\n    var loud: Boolean?\n}"), "{types}");
    assert!(!dir.path().join("out/acme/greeter/Default.kt").exists());
}

#[test]
fn a_package_with_only_subpath_exports_gets_a_kotlin_package_per_subpath() {
    // Like `cubing`: no `"."` entry, so `import "cubing"` fails and each subpath is a module.
    let (dir, out) = generated(&["cubing"], |nm| {
        write(
            nm,
            "cubing/package.json",
            r#"{"name":"cubing","exports":{
                "./alg":{"types":"./dist/lib/cubing/alg/index.d.ts","import":"./dist/lib/cubing/alg/index.js"},
                "./scramble":{"types":"./dist/lib/cubing/scramble/index.d.ts","import":"./dist/lib/cubing/scramble/index.js"},
                "./untyped":"./dist/untyped.js",
                "./package.json":"./package.json"
            }}"#,
        );
        write(nm, "cubing/dist/lib/cubing/alg/index.d.ts", "export { Alg, experimentalAppendMove } from '../chunks/alg';\n");
        write(
            nm,
            "cubing/dist/lib/cubing/chunks/alg.d.ts",
            "export declare class Alg {\n    constructor(alg?: string);\n    toString(): string;\n}\nexport declare function experimentalAppendMove(alg: Alg, move: string): Alg;\n",
        );
        write(
            nm,
            "cubing/dist/lib/cubing/scramble/index.d.ts",
            "import { Alg } from '../chunks/alg';\ndeclare function randomScrambleForEvent(eventID: string): Promise<Alg>;\nexport { randomScrambleForEvent };\n",
        );
    });
    assert!(out.untyped.is_empty());
    assert!(has_declarations(&dir.path().join("node_modules"), "cubing"));
    let alg = read(&dir, "cubing/alg/Exports.kt");
    assert!(alg.contains("@file:JsModule(\"cubing/alg\")\n\npackage cubing.alg"), "{alg}");
    assert!(alg.contains("external class Alg : JsAny {\n    constructor(alg: String = definedExternally)"), "{alg}");
    assert!(alg.contains("external fun experimentalAppendMove(alg: cubing.alg.Alg, move: String): cubing.alg.Alg"), "{alg}");
    let scramble = read(&dir, "cubing/scramble/Exports.kt");
    assert!(scramble.contains("@file:JsModule(\"cubing/scramble\")\n\npackage cubing.scramble"), "{scramble}");
    assert!(scramble.contains("external fun randomScrambleForEvent(eventID: String): Promise<cubing.alg.Alg>"), "{scramble}");
    // Nothing is bound to the package root, which cannot be imported.
    assert!(!dir.path().join("out/cubing/Exports.kt").exists());
}

#[test]
fn a_package_with_its_own_types_wins_over_types_packages() {
    let dir = tempfile::tempdir().unwrap();
    let nm = dir.path().join("node_modules");
    write(&nm, "left-pad/package.json", r#"{"name":"left-pad","types":"index.d.ts"}"#);
    write(
        &nm,
        "left-pad/index.d.ts",
        "declare function leftPad(str: string | number, len: number, ch?: string): string;\nexport = leftPad;\n",
    );
    assert!(has_declarations(&nm, "left-pad"));
    assert!(!has_declarations(&nm, "right-pad"));
    let out = generate(&["left-pad".to_owned()], &[nm], &dir.path().join("out")).unwrap();
    let default = std::fs::read_to_string(&out.files[0]).unwrap();
    assert!(
        default.contains("@JsModule(\"left-pad\")\nexternal fun leftPad(str: String, len: Int, ch: String = definedExternally): String"),
        "{default}"
    );
    assert!(default.contains("external fun leftPad(str: Double, len: Double, ch: String = definedExternally): String"), "{default}");
}

#[test]
fn many_optional_numbers_keep_their_parameters() {
    let dir = tempfile::tempdir().unwrap();
    let nm = dir.path().join("node_modules");
    write(&nm, "shapes/package.json", r#"{"name":"shapes","types":"index.d.ts"}"#);
    write(
        &nm,
        "shapes/index.d.ts",
        "export class Box {\n  constructor(width?: number, height?: number, depth?: number, a?: number, b?: number, c?: number);\n}\n\
         export class Vec {\n  constructor(x?: number, y?: number, z?: number);\n}\n",
    );
    let out = generate(&["shapes".to_owned()], &[nm], &dir.path().join("out")).unwrap();
    let exports = std::fs::read_to_string(&out.files[0]).unwrap();
    assert!(
        exports.contains(
            "constructor(width: Double = definedExternally, height: Double = definedExternally, depth: Double = definedExternally, \
             a: Double = definedExternally, b: Double = definedExternally, c: Double = definedExternally)"
        ),
        "{exports}"
    );
    assert!(
        exports.contains("constructor(x: Double = definedExternally, y: Double = definedExternally, z: Double = definedExternally)"),
        "{exports}"
    );
}

/// The project's own modules: `js/sum.ts` with bodies and an arrow function, `js/shapes/point.ts`
/// that it imports, and the types of a JavaScript module.
fn local_modules(root: &Path) -> Vec<LocalModule> {
    write(
        root,
        "js/sum.ts",
        r#"
        import { Point } from "./shapes/point.js";
        import express from "express";

        /** Adds two numbers. */
        export function sum(a: number, b: number): number {
            if (a < b && b > 0) { return b + a; }
            for (let i = 0; i < 3; i++) { console.log(`${i} {`); }
            return a + b;
        }
        export const twice = (p: Point): Point => ({ x: p.x * 2, y: p.y < 0 ? 0 : p.y * 2 });
        export const guess = (a) => a;
        export function app(): express.Express { return express(); }
        const hidden = 1 < 2;
        export class Counter {
            count: number = 0;
            constructor(start: number) { this.count = start < 0 ? 0 : start; }
            increment(by: number): number { this.count += by; return this.count; }
            get doubled(): number { return this.count * 2; }
        }
        "#,
    );
    write(
        root,
        "js/shapes/point.ts",
        "export interface Point { x: number; y: number }\nexport function origin(): Point { return { x: 0, y: 0 }; }\n",
    );
    write(root, "js/legacy.js", "export function shout(s) { return s.toUpperCase(); }\n");
    write(root, "js/legacy.d.ts", "export declare function shout(text: string): string;\n");
    let module = |file: &str, package: &str, spec: &str| LocalModule {
        file: root.join(file),
        display: file.to_owned(),
        package: package.to_owned(),
        spec: spec.to_owned(),
    };
    vec![
        module("js/sum.ts", "js.sum", "./js/sum.js"),
        module("js/shapes/point.ts", "js.shapes.point", "./js/shapes/point.js"),
        module("js/legacy.d.ts", "js.legacy", "./js/legacy.js"),
    ]
}

#[test]
fn the_projects_own_modules_get_declarations() {
    let dir = tempfile::tempdir().unwrap();
    let modules = local_modules(dir.path());
    let out = dir.path().join("out");
    let generated = generate_local(&modules, &[], &out).unwrap();
    assert_eq!(generated.files.len() + 1, generated.changed.len(), "all new, with the module of `Point`'s factory");
    assert_eq!(generated.untyped, [("js/sum.ts".to_owned(), "guess".to_owned())]);

    let sum = std::fs::read_to_string(out.join("js/sum/Exports.kt")).unwrap();
    assert!(sum.starts_with("// Generated by Kargo from the TypeScript declarations of `js/sum.ts`."), "{sum}");
    assert!(sum.contains("@file:JsModule(\"./js/sum.js\")\n\npackage js.sum\n"), "{sum}");
    assert!(sum.contains("/**\n * Adds two numbers.\n */\nexternal fun sum(a: Int, b: Int): Double"), "{sum}");
    assert!(sum.contains("external fun twice(p: js.shapes.point.Point): js.shapes.point.Point"), "{sum}");
    assert!(sum.contains("external fun guess(a: JsAny?): JsAny?"), "{sum}");
    // npm packages get declarations of their own.
    assert!(sum.contains("external fun app(): JsAny"), "{sum}");
    assert!(!sum.contains("hidden"), "{sum}");
    let counter = body(&sum, "external class Counter : JsAny {");
    assert!(counter.contains("    constructor(start: Int)\n"), "{counter}");
    assert!(counter.contains("    var count: Double\n"), "{counter}");
    assert!(counter.contains("    val doubled: Double\n"), "{counter}");
    assert!(counter.contains("    fun increment(by: Int): Double\n"), "{counter}");

    let point = std::fs::read_to_string(out.join("js/shapes/point/Types.kt")).unwrap();
    assert!(point.contains("package js.shapes.point"), "{point}");
    assert!(point.contains("external interface Point : JsAny {\n    var x: Double\n    var y: Double\n}"), "{point}");
    let factory = std::fs::read_to_string(out.join("js/shapes/point/Factories.kt")).unwrap();
    assert!(factory.contains("@file:JsModule(\"./js/kargo-factories/js.shapes.point.mjs\")\n\npackage js.shapes.point\n"), "{factory}");
    assert!(factory.contains("external fun Point(x: Int, y: Int): js.shapes.point.Point\n"), "{factory}");
    assert!(factory.contains("external fun Point(x: Double, y: Double): js.shapes.point.Point\n"), "{factory}");
    let module = out.join("kargo-factories/js.shapes.point.mjs");
    assert_eq!(factory_modules(&out), [("kargo-factories/js.shapes.point.mjs".to_owned(), module.clone())]);
    let js = std::fs::read_to_string(&module).unwrap();
    assert!(
        js.contains("export function Point(x, y) {\n    const $o = {};\n    $o[\"x\"] = x;\n    $o[\"y\"] = y;\n    return $o;\n}"),
        "{js}"
    );
    let origin = std::fs::read_to_string(out.join("js/shapes/point/Exports.kt")).unwrap();
    assert!(origin.contains("@file:JsModule(\"./js/shapes/point.js\")"), "{origin}");
    assert!(origin.contains("external fun origin(): js.shapes.point.Point"), "{origin}");
    let legacy = std::fs::read_to_string(out.join("js/legacy/Exports.kt")).unwrap();
    assert!(
        legacy.contains("@file:JsModule(\"./js/legacy.js\")") && legacy.contains("external fun shout(text: String): String"),
        "{legacy}"
    );
}

#[test]
fn only_changed_declarations_are_written_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut modules = local_modules(dir.path());
    let out = dir.path().join("out");
    generate_local(&modules, &[], &out).unwrap();
    assert!(generate_local(&modules, &[], &out).unwrap().changed.is_empty(), "nothing changed");

    // A function added to one file rewrites that file's declarations only.
    write(
        dir.path(),
        "js/shapes/point.ts",
        "export interface Point { x: number; y: number }\nexport function origin(): Point { return { x: 0, y: 0 }; }\nexport function unit(): Point { return { x: 1, y: 1 }; }\n",
    );
    let generated = generate_local(&modules, &[], &out).unwrap();
    assert_eq!(generated.changed, [out.join("js/shapes/point/Exports.kt")]);
    assert!(std::fs::read_to_string(&generated.changed[0]).unwrap().contains("external fun unit(): js.shapes.point.Point"));

    // A removed module takes its declarations with it.
    modules.retain(|m| m.package != "js.legacy");
    let generated = generate_local(&modules, &[], &out).unwrap();
    assert_eq!(generated.changed, [out.join("js/legacy/Exports.kt")]);
    assert!(!out.join("js/legacy/Exports.kt").exists());
}

#[test]
fn object_types_get_interfaces_and_factories() {
    let (dir, out) = generated(&["widgets"], |nm| {
        write(nm, "widgets/package.json", r#"{"name":"widgets","types":"index.d.ts"}"#);
        write(
            nm,
            "widgets/index.d.ts",
            r#"
            export interface Options { label?: string; size: number; "data-id"?: string; onClick?: (x: number) => void }
            export interface Handle { close(): void; id: string }
            export interface Indexed { [key: string]: number }
            export interface Empty {}
            export declare function doSomething(opts: { a: number; b?: { deep: boolean } }): void;
            export declare function make(options: Options): Handle;
            export declare function anything(value: {}): void;
            export type Theme = { dark: boolean };
            export declare function theme(): Theme;
            export declare function Theme(dark: boolean): Theme;
            "#,
        );
    });
    assert!(out.untyped.is_empty());
    let types = read(&dir, "widgets/Types.kt");
    let exports = read(&dir, "widgets/Exports.kt");
    let factories = read(&dir, "widgets/Factories.kt");
    let js = read(&dir, "kargo-factories/widgets.mjs");

    // An inline object type is an interface named after the function and parameter, and nested ones after the property.
    assert!(exports.contains("external fun doSomething(opts: widgets.DoSomethingOpts): Unit"), "{exports}");
    assert!(
        types.contains("external interface DoSomethingOpts : JsAny {\n    var a: Double\n    var b: widgets.DoSomethingOptsB?\n}"),
        "{types}"
    );
    assert!(types.contains("external interface DoSomethingOptsB : JsAny {\n    var deep: Boolean\n}"), "{types}");
    assert!(factories.contains("@file:JsModule(\"./js/kargo-factories/widgets.mjs\")\n\npackage widgets\n"), "{factories}");
    for number in ["Int", "Double"] {
        let factory = format!(
            "external fun DoSomethingOpts(a: {number}, b: widgets.DoSomethingOptsB = definedExternally): widgets.DoSomethingOpts\n"
        );
        assert!(factories.contains(&factory), "{factories}");
    }
    assert!(factories.contains("external fun DoSomethingOptsB(deep: Boolean): widgets.DoSomethingOptsB\n"), "{factories}");
    // `{}` is any object.
    assert!(exports.contains("external fun anything(value: JsAny): Unit"), "{exports}");

    // Required properties come first; an optional one is set only when given; a JavaScript name Kotlin can't
    // spell is a positional parameter; a callback property takes a lambda.
    let optional = "label: String = definedExternally, p2: String = definedExternally";
    for factory in [
        format!("external fun Options(size: Int, {optional}): widgets.Options\n"),
        format!("external fun Options(size: Double, {optional}): widgets.Options\n"),
        format!("external fun Options(size: Int, {optional}, onClick: (Double) -> Unit): widgets.Options\n"),
    ] {
        assert!(factories.contains(&factory), "{factories}");
    }
    assert!(
        js.contains(
            "export function Options(size, label, p2, onClick) {\n    const $o = {};\n    $o[\"size\"] = size;\n    \
             if (label !== undefined) $o[\"label\"] = label;\n    if (p2 !== undefined) $o[\"data-id\"] = p2;\n    \
             if (onClick !== undefined) $o[\"onClick\"] = onClick;\n    return $o;\n}"
        ),
        "{js}"
    );
    // Objects with methods or index signatures, and empty ones, get none.
    assert!(!factories.contains("fun Handle("), "{factories}");
    assert!(!factories.contains("fun Indexed("), "{factories}");
    assert!(!js.contains("function Handle(") && !js.contains("function Indexed("), "{js}");
    // A type alias of an object type keeps its name; a function of the package with the same signature wins.
    assert!(types.contains("external interface Theme : JsAny {\n    var dark: Boolean\n}"), "{types}");
    assert!(exports.contains("external fun Theme(dark: Boolean): widgets.Theme"), "{exports}");
    assert!(!factories.contains("fun Theme("), "{factories}");
}
