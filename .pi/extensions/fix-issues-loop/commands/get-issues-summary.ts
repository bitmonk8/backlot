import type { ExtensionAPI, ExtensionCommandContext } from "@mariozechner/pi-coding-agent";

interface GhIssue {
  number: number;
  title: string;
  labels: Array<{ name: string }>;
  state: string;
}

export interface IssueSummaryItem {
  number: number;
  title: string;
  crate: string | null;
  importance: string | null;
  effort: string | null;
  type: string | null;
}

export interface IssuesSummary {
  count: number;
  issues: IssueSummaryItem[];
}

function parseLabel(labels: Array<{ name: string }>, prefix: string): string | null {
  const label = labels.find((l) => l.name.startsWith(prefix + ":"));
  return label ? label.name.slice(prefix.length + 1) : null;
}

function parseIssues(raw: GhIssue[]): IssuesSummary {
  const issues = raw.map((issue) => ({
    number: issue.number,
    title: issue.title,
    crate: parseLabel(issue.labels, "crate"),
    importance: parseLabel(issue.labels, "importance"),
    effort: parseLabel(issue.labels, "effort"),
    type: parseLabel(issue.labels, "type"),
  }));
  return { count: issues.length, issues };
}

export function registerGetIssuesSummary(pi: ExtensionAPI) {
  pi.registerCommand("get-issues-summary", {
    description: "Fetch open GitHub issues and print structured JSON summary",
    handler: async (args: string, ctx: ExtensionCommandContext) => {
      const labelArgs: string[] = [];
      if (args) {
        for (const part of args.split(/\s+/)) {
          if (part.startsWith("--crate=")) labelArgs.push("-l", `crate:${part.slice(8)}`);
          else if (part.startsWith("--importance=")) labelArgs.push("-l", `importance:${part.slice(13)}`);
          else if (part.startsWith("--effort=")) labelArgs.push("-l", `effort:${part.slice(9)}`);
          else if (part.startsWith("--type=")) labelArgs.push("-l", `type:${part.slice(7)}`);
        }
      }

      const result = await pi.exec(
        "gh",
        [
          "issue", "list",
          "--repo", "bitmonk8/backlot",
          "--state", "open",
          "--json", "number,title,labels,state",
          "--limit", "5000",
          ...labelArgs,
        ],
        { timeout: 30000 }
      );

      if (result.code !== 0) {
        console.error(`gh issue list failed (exit ${result.code}): ${result.stderr}`);
        process.exit(1);
      }

      const raw: GhIssue[] = JSON.parse(result.stdout);
      const summary = parseIssues(raw);
      process.stdout.write(JSON.stringify(summary) + "\n");
    },
  });
}
