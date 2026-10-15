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
