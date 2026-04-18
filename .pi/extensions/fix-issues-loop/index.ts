import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";
import { registerGetIssuesSummary } from "./commands/get-issues-summary";
import { registerPickIssue } from "./commands/pick-issue";

export default function (pi: ExtensionAPI) {
  registerGetIssuesSummary(pi);
  registerPickIssue(pi);
}
