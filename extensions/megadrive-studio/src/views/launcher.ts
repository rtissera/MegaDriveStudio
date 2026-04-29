// SPDX-License-Identifier: MIT
import * as vscode from "vscode";

/** Tree-view stub whose only role is to host a single "Open …" entry that fires a command. */
export class LauncherProvider implements vscode.TreeDataProvider<vscode.TreeItem> {
  readonly onDidChangeTreeData = new vscode.EventEmitter<void>().event;
  constructor(private label: string, private commandId: string, private commandTitle: string) {}
  getTreeItem(el: vscode.TreeItem): vscode.TreeItem { return el; }
  getChildren(): vscode.TreeItem[] {
    const item = new vscode.TreeItem(this.label, vscode.TreeItemCollapsibleState.None);
    item.command = { command: this.commandId, title: this.commandTitle };
    item.iconPath = new vscode.ThemeIcon("window");
    return [item];
  }
}
