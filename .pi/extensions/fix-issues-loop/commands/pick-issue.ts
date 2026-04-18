import type { ExtensionAPI, ExtensionCommandContext } from "@mariozechner/pi-coding-agent";

interface GhIssue {
  number: number;
  title: string;
  labels: Array<{ name: string }>;
  state: string;
}

export interface PickResult {
  number: number | null;
  title: string | null;
  crate: string | null;
  importance: string | null;
  effort: string | null;
  type: string | null;
  url: string | null;
}

function parseLabel(labels: Array<{ name: string }>, prefix: string): string | null {
  const label = labels.find((l) => l.name.startsWith(prefix + ":"));
  return label ? label.name.slice(prefix.length + 1) : null;
}

const IMPORTANCE_ORDER: Record<string, number> = { high: 0, medium: 1, low: 2 };
const EFFORT_ORDER: Record<string, number> = { low: 0, medium: 1, high: 2 };

function prioritize(issues: GhIssue[]): GhIssue[] {
  return [...issues].sort((a, b) => {
    const aImp = IMPORTANCE_ORDER[parseLabel(a.labels, "importance") ?? ""] ?? 3;
    const bImp = IMPORTANCE_ORDER[parseLabel(b.labels, "importance") ?? ""] ?? 3;
    if (aImp !== bImp) return aImp - bImp;

    const aEff = EFFORT_ORDER[parseLabel(a.labels, "effort") ?? ""] ?? 3;
    const bEff = EFFORT_ORDER[parseLabel(b.labels, "effort") ?? ""] ?? 3;
    if (aEff !== bEff) return aEff - bEff;

    const aIsBug = parseLabel(a.labels, "type") === "bug" ? 0 : 1;
    const bIsBug = parseLabel(b.labels, "type") === "bug" ? 0 : 1;
    if (aIsBug !== bIsBug) return aIsBug - bIsBug;

    return a.number - b.number;
  });
}

export function registerPickIssue(pi: ExtensionAPI) {
  pi.registerCommand("pick-issue", {
    description: "Pick highest-priority open issue. Args: [--crate=X] [--skip=1,2,3] [--importance=X] [--effort=X] [--type=X]",
    handler: async (args: string, ctx: ExtensionCommandContext) => {
      let crateFilter: string | null = null;
      let importanceFilter: string | null = null;
      let effortFilter: string | null = null;
      let typeFilter: string | null = null;
      const skipSet = new Set<number>();

      if (args) {
        for (const part of args.split(/\s+/)) {
          if (part.startsWith("--crate=")) crateFilter = part.slice(8);
          else if (part.startsWith("--skip=")) {
            for (const n of part.slice(7).split(",")) {
              const num = parseInt(n, 10);
              if (!isNaN(num)) skipSet.add(num);
            }
          }
          else if (part.startsWith("--importance=")) importanceFilter = part.slice(13);
          else if (part.startsWith("--effort=")) effortFilter = part.slice(9);
          else if (part.startsWith("--type=")) typeFilter = part.slice(7);
        }
      }

      const labelArgs: string[] = [];
      if (crateFilter) labelArgs.push("-l", `crate:${crateFilter}`);
      if (importanceFilter) labelArgs.push("-l", `importance:${importanceFilter}`);
      if (effortFilter) labelArgs.push("-l", `effort:${effortFilter}`);
      if (typeFilter) labelArgs.push("-l", `type:${typeFilter}`);

      const result = await pi.exec(
        "gh",
        [
          "issue", "list",
          "--repo", "bitmonk8/backlot",
          "--state", "open",
          "--json", "number,title,labels,state",
          "--limit", "100",
          ...labelArgs,
        ],
        { timeout: 30000 }
      );

      if (result.code !== 0) {
        console.error(`gh issue list failed (exit ${result.code}): ${result.stderr}`);
        process.exit(1);
      }

      const raw: GhIssue[] = JSON.parse(result.stdout);
      const filtered = raw.filter((issue) => !skipSet.has(issue.number));
      const sorted = prioritize(filtered);

      let pick: PickResult;
      if (sorted.length === 0) {
        pick = { number: null, title: null, crate: null, importance: null, effort: null, type: null, url: null };
      } else {
        const best = sorted[0];
        pick = {
          number: best.number,
          title: best.title,
          crate: parseLabel(best.labels, "crate"),
          importance: parseLabel(best.labels, "importance"),
          effort: parseLabel(best.labels, "effort"),
          type: parseLabel(best.labels, "type"),
          url: `https://github.com/bitmonk8/backlot/issues/${best.number}`,
        };
      }

      process.stdout.write(JSON.stringify(pick) + "\n");
    },
  });
}
