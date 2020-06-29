import { ExtensionContext } from "vscode";
import { LanguageClient } from "vscode-languageclient";
import * as path from "path";

let client: LanguageClient | null;

export function activate(cx: ExtensionContext) {
  if (client !== null) {
    console.log("millet-ls: error: cannot re-activate");
    return;
  }
  console.log("millet-ls: activate");
  const serverOpts = {
    command: cx.asAbsolutePath(path.join("out", "millet-ls")),
  };
  const clientOpts = {
    documentSelector: [{ scheme: "file", language: "sml" }],
  };
  client = new LanguageClient("millet-ls", serverOpts, clientOpts, true);
  cx.subscriptions.push(client.start());
}

export function deactivate(): Promise<void> {
  return client === null ? Promise.resolve() : client.stop();
}
