// Unit tests for the `apg_json` tool's query and CSV formatting
// (`opencode-suite/tools/apg_json.ts`). Pure string/row composition — no
// database, no subprocess — so plain `bun test` runs them.
import { test, expect } from "bun:test"

import { jsonTreeQuery, pointerUnder, formatJsonTree } from "../tools/apg_json.ts"

const header = ["s.fqn", "s.start_line", "s.end_line", "s.properties"]

test("the query selects the file's pointer-bearing Structs in line order", () => {
  const q = jsonTreeQuery("cfg/app.json")
  expect(q).toContain("s.path = 'cfg/app.json'")
  expect(q).toContain(`s.properties CONTAINS '"pointer"'`)
  expect(q.endsWith("ORDER BY s.start_line, s.start")).toBe(true)
})

test("pointer prefixes match whole reference tokens", () => {
  expect(pointerUnder("/a", "/a")).toBe(true)
  expect(pointerUnder("/a/b", "/a")).toBe(true)
  expect(pointerUnder("/a/b", "/a/")).toBe(true)
  expect(pointerUnder("/ab", "/a")).toBe(false)
  expect(pointerUnder("/x", "")).toBe(true)
})

test("rows format as fqn,lines,pointer,type,value with markers and filtering", () => {
  const rows = [
    header,
    ["json.cfg.app.json.printer", "2", "5", JSON.stringify({ pointer: "/printer", type: "object" })],
    ["json.cfg.app.json.printer.name", "3", "3", JSON.stringify({ pointer: "/printer/name", type: "string", value: "a, b" })],
    ["json.cfg.app.json.printer.deep", "4", "4", JSON.stringify({ pointer: "/printer/deep", type: "object", truncated: "depth" })],
    ["json.cfg.app.json.tags", "6", "6", JSON.stringify({ pointer: "/tags", type: "array", items: "[1,2]" })],
  ]
  expect(formatJsonTree(rows).split("\n")).toEqual([
    "fqn,lines,pointer,type,value",
    "json.cfg.app.json.printer,2-5,/printer,object,",
    'json.cfg.app.json.printer.name,3,/printer/name,string,"a, b"',
    "json.cfg.app.json.printer.deep,4,/printer/deep,object,[truncated=depth]",
    "json.cfg.app.json.tags,6,/tags,array,\"[1,2]\"",
  ])
  expect(formatJsonTree(rows, "/printer").split("\n")).toHaveLength(4)
  expect(formatJsonTree(rows, "/nope").split("\n")).toEqual(["fqn,lines,pointer,type,value"])
})
