import assert from "node:assert/strict";
import { test } from "node:test";
import { labelsForTitle } from "./issue-labels.mjs";

test("fix and bug prefixes add bug", () => {
    assert.deepEqual(labelsForTitle("fix: conserve cash"), ["bug"]);
    assert.deepEqual(labelsForTitle("bug: broken total"), ["bug"]);
    assert.deepEqual(labelsForTitle("Fix: case does not matter"), ["bug"]);
    assert.deepEqual(labelsForTitle("fix(wallet): replay"), ["bug"]);
});

test("docs and feat prefixes", () => {
    assert.deepEqual(labelsForTitle("docs: reconcile the index"), ["documentation"]);
    assert.deepEqual(labelsForTitle("feat: add a rail"), ["enhancement"]);
    assert.deepEqual(labelsForTitle("feat(web): portal editor"), ["enhancement"]);
});

test("Security prefix adds security and nothing else", () => {
    assert.deepEqual(labelsForTitle("Security: class of problem"), ["security"]);
    assert.deepEqual(labelsForTitle("security: header shape"), ["security"]);
});

test("acceptance ids and operator launch combine with the type label", () => {
    assert.deepEqual(
        labelsForTitle("feat: Complete embedded application integration (N21)"),
        ["enhancement", "operator-launch"],
    );
    assert.deepEqual(
        labelsForTitle("docs: Two independent operator pilots (N19)"),
        ["documentation", "operator-launch"],
    );
    assert.deepEqual(
        labelsForTitle("test: Final integrated candidate acceptance (N18)"),
        ["operator-launch"],
    );
    assert.deepEqual(labelsForTitle("chore: follow #725"), ["operator-launch"]);
    assert.deepEqual(labelsForTitle("Support the operator-launch checklist"), ["operator-launch"]);
});

test("test and chore alone add no label", () => {
    assert.deepEqual(labelsForTitle("test: check the parser"), []);
    assert.deepEqual(labelsForTitle("chore: tidy comments"), []);
});

test("a prefix that is not at the start does not match", () => {
    assert.deepEqual(labelsForTitle("Please fix: the docs later"), []);
    assert.deepEqual(labelsForTitle("Notes on security: later"), []);
});
