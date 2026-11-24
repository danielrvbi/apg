import { Plugin } from "@opencode/plugin"
import path from "node:path"
import { runCypher, lit, findApgRoot, resolveProjectPath, csvToRows, type ToolContext } from "../lib/apg.ts"

type ApgJsonInput = { directory?: string; path: string; pointer?: string }

/** The Cypher listing one JSON file's tree nodes (the structural `json`
 *  stream's Structs, the ones carrying a `pointer` property) in line order. */
export function jsonTreeQuery(storedPath: string): string {
  return (
    `MATCH (s:Struct) WHERE s.path = ${lit(storedPath)} AND s.properties CONTAINS '"pointer"' ` +
    `RETURN s.fqn, s.start_line, s.end_line, s.properties ORDER BY s.start_line, s.start`
  )
}

/** True when RFC 6901 `pointer` is `prefix` or lies beneath it (`/a` covers
 *  `/a` and `/a/b`, never `/ab`). An empty prefix covers everything. */
export function pointerUnder(pointer: string, prefix: string): boolean {
  const p = prefix.endsWith("/") ? prefix.slice(0, -1) : prefix
  if (p === "") return true
  return pointer === p || pointer.startsWith(p + "/")
}

function csvCell(cell: string): string {
  return /[",\n\r]/.test(cell) ? `"${cell.replace(/"/g, '""')}"` : cell
}

/** Formats the query rows (header first) as `fqn,lines,pointer,type,value`
 *  CSV, filtered to `pointerPrefix`. `value` is a scalar's value, else a
 *  list's `items`; a `truncated` marker is appended as ` [truncated=…]` so an
 *  agent knows to read those lines itself. */
export function formatJsonTree(rows: string[][], pointerPrefix = ""): string {
  const out = ["fqn,lines,pointer,type,value"]
  for (const [fqn, sl, el, rawProps] of rows.slice(1)) {
    let props: Record<string, string> = {}
    try {
      props = JSON.parse(rawProps || "{}")
    } catch {
      continue
    }
    const pointer = props.pointer ?? ""
    if (!pointerUnder(pointer, pointerPrefix)) continue
    const lines = sl === el ? sl : `${sl}-${el}`
    let value = props.value ?? props.items ?? ""
    if (props.truncated) value = `${value}${value ? " " : ""}[truncated=${props.truncated}]`
    out.push([fqn, lines, pointer, props.type ?? "", value].map(csvCell).join(","))
  }
  return out.join("\n")
}

export default Plugin.define({
  id: "apg_json",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_json",
        options: { codemode: false },
        description:
          "Show a JSON/JSONC file's graphed tree in line order as CSV (fqn,lines,pointer,type,value): objects, scalar values and list items, each with its RFC 6901 pointer. Filter with a pointer prefix (e.g. /compilerOptions). A [truncated=…] marker means a depth/item/node limit cut the tree there — read those lines directly.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            path: {
              type: "string",
              description: "Path of the JSON file, absolute or repo-relative, e.g. /abs/tsconfig.json (required)",
            },
            pointer: {
              type: "string",
              description: "Optional RFC 6901 pointer prefix, e.g. /printer — only nodes at or beneath it are listed",
            },
          },
          required: ["path"],
          additionalProperties: false,
        },
        async execute(raw, context) {
          const input = raw as ApgJsonInput
          const merged: ToolContext = {
            agent: context.agent,
            sessionID: context.sessionID,
            messageID: context.messageID,
            id: context.id,
            location: ctx.location,
          }
          if (!input.path) return { content: "Error: path is required" }
          // The graph stores repo-relative identities; convert an absolute path.
          const root = findApgRoot(merged, input.directory)
          const stored = path.isAbsolute(input.path) && root ? resolveProjectPath(root, input.path) : input.path
          const out = await runCypher(merged, jsonTreeQuery(stored), input.directory)
          let rows: string[][]
          try {
            rows = csvToRows(out)
          } catch (e) {
            return { content: (e as Error).message }
          }
          const body = formatJsonTree(rows, input.pointer ?? "")
          if (body.split("\n").length === 1) {
            return {
              content: `${body}\n(no JSON tree nodes for ${stored}${input.pointer ? ` under ${input.pointer}` : ""} — is it a scanned .json/.jsonc file?)`,
            }
          }
          return { content: body }
        },
      })
    })
  },
})
