window.DEPS_INDEX = __DEPS_INDEX__;

window.searchDeps = function (query) {
  "use strict";
  var value = String(query || "").trim().toLowerCase();
  if (!value) return [];
  var entries = window.DEPS_INDEX || [];
  var matches = [];
  for (var index = 0; index < entries.length; index += 1) {
    var entry = entries[index];
    var haystack = (entry.name + " " + entry.ecosystem + " " + entry.purl).toLowerCase();
    if (haystack.indexOf(value) >= 0) {
      matches.push(entry);
      if (matches.length >= 40) break;
    }
  }
  matches.sort(function (left, right) {
    return left.name.localeCompare(right.name);
  });
  return matches;
};
