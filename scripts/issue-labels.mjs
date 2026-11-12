/**
 * Labels to add for an issue title.
 *
 * The result is only an add-list. Callers must not remove labels that are
 * already on the issue. `test:` and `chore:` have no type label in this
 * repository; they still receive `operator-launch` when the title names an
 * acceptance unit.
 *
 * @param {string} title
 * @returns {string[]}
 */
export function labelsForTitle(title) {
    const text = title ?? "";
    const labels = [];
    if (/^(?:fix|bug)(?:\([^)]*\))?:/i.test(text)) {
        labels.push("bug");
    }
    if (/^docs(?:\([^)]*\))?:/i.test(text)) {
        labels.push("documentation");
    }
    if (/^feat(?:\([^)]*\))?:/i.test(text)) {
        labels.push("enhancement");
    }
    if (/^security:/i.test(text)) {
        labels.push("security");
    }
    if (isOperatorLaunch(text)) {
        labels.push("operator-launch");
    }
    return labels;
}

/**
 * @param {string} title
 * @returns {boolean}
 */
function isOperatorLaunch(title) {
    return /\bN\d+\b/.test(title)
        || /#725\b/.test(title)
        || /operator[\s-]launch/i.test(title);
}
