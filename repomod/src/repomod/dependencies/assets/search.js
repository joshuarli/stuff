"use strict";
window.DEPS_INDEX = __DEPS_INDEX__;
window.searchDeps = (query) => {
    "use strict";
    const value = String(query).trim().toLowerCase();
    if (value === "")
        return [];
    const entries = window.DEPS_INDEX ?? [];
    const matches = [];
    for (const entry of entries) {
        const haystack = entry.name + " " + entry.ecosystem + " " + entry.purl;
        if (haystack.toLowerCase().includes(value)) {
            matches.push(entry);
            if (matches.length >= 40)
                break;
        }
    }
    matches.sort((left, right) => left.name.localeCompare(right.name));
    return matches;
};
