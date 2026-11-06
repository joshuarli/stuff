(function () {
  "use strict";
  function pageData() {
    var node = document.getElementById("page-data");
    if (!node) return {};
    try {
      return JSON.parse(node.textContent || "{}");
    } catch (error) {
      return {};
    }
  }
  var data = pageData();
  var page = document.body.getAttribute("data-page") || "";
  var prefix = document.body.getAttribute("data-prefix") || "";

  document.addEventListener("keydown", function (event) {
    var target = event.target;
    var typing = target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable);
    if (event.key === "/" && !typing) {
      var search = document.getElementById("search");
      if (search) {
        event.preventDefault();
        search.focus();
      }
      return;
    }
    if (typing) return;
    if (event.key === "j" || event.key === "k") {
      var rows = Array.prototype.slice.call(document.querySelectorAll("tr[data-row], .row[data-row]"));
      if (!rows.length) return;
      event.preventDefault();
      var index = rows.findIndex(function (row) { return row.classList.contains("selected"); });
      var next = index < 0 ? 0 : event.key === "j" ? Math.min(rows.length - 1, index + 1) : Math.max(0, index - 1);
      rows.forEach(function (row) { row.classList.remove("selected"); });
      rows[next].classList.add("selected");
      rows[next].scrollIntoView({ block: "nearest" });
    }
    if (event.key === "Enter" && !typing) {
      var selected = document.querySelector(".selected[data-href]");
      if (selected) window.location.href = selected.getAttribute("data-href");
    }
  });

  document.addEventListener("click", function (event) {
    var button = event.target.closest("[data-copy]");
    if (!button) return;
    var value = button.getAttribute("data-copy") || "";
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(value).then(function () {
        button.setAttribute("data-copied", "1");
        button.textContent = "copied";
        window.setTimeout(function () {
          button.removeAttribute("data-copied");
          button.textContent = "copy";
        }, 1200);
      });
    }
  });

  var input = document.getElementById("search");
  var results = document.getElementById("search-results");
  function closeResults() {
    if (!results) return;
    results.innerHTML = "";
    results.style.display = "none";
  }
  function renderResults() {
    if (!input || !results || typeof window.searchDeps !== "function") return;
    var found = window.searchDeps(input.value);
    results.innerHTML = "";
    if (!found.length) {
      closeResults();
      return;
    }
    found.forEach(function (entry) {
      var row = document.createElement("div");
      var link = document.createElement("a");
      link.href = prefix + "packages.html#pkg=" + encodeURIComponent(entry.purl);
      link.textContent = entry.label + " @ " + entry.ecosystem;
      var meta = document.createElement("span");
      meta.className = "meta";
      meta.textContent = "  " + entry.purl;
      link.appendChild(meta);
      row.appendChild(link);
      if (page === "landscape") {
        var add = document.createElement("button");
        add.type = "button";
        add.textContent = "+ column";
        add.setAttribute("data-add-column", entry.purl);
        row.appendChild(add);
      }
      results.appendChild(row);
    });
    results.style.display = "block";
  }
  if (input) {
    input.addEventListener("input", renderResults);
    input.addEventListener("focus", renderResults);
    document.addEventListener("click", function (event) {
      if (event.target === input || (results && results.contains(event.target))) return;
      closeResults();
    });
  }

  function routePackage() {
    if (page !== "packages") return;
    var sections = Array.prototype.slice.call(document.querySelectorAll(".package"));
    var list = document.getElementById("package-index");
    var match = null;
    var hash = window.location.hash || "";
    if (hash.indexOf("#pkg=") === 0) {
      var wanted = decodeURIComponent(hash.slice(5));
      sections.forEach(function (section) {
        var active = section.getAttribute("data-purl") === wanted;
        section.classList.toggle("active", active);
        if (active) match = section;
      });
    }
    if (list) list.style.display = match ? "none" : "block";
    if (match) match.scrollIntoView({ block: "start" });
  }
  window.addEventListener("hashchange", routePackage);
  routePackage();

  function sortRows(key, direction) {
    var table = document.querySelector("table.landscape");
    if (!table) return;
    var body = table.tBodies[0];
    var rows = Array.prototype.slice.call(body.rows);
    rows.sort(function (left, right) {
      var a = left.getAttribute("data-" + key) || "";
      var b = right.getAttribute("data-" + key) || "";
      if (key === "commonality" || key === "coverage") {
        a = Number(a);
        b = Number(b);
      }
      if (a < b) return direction === "asc" ? -1 : 1;
      if (a > b) return direction === "asc" ? 1 : -1;
      return (left.getAttribute("data-repo") || "").localeCompare(right.getAttribute("data-repo") || "");
    });
    rows.forEach(function (row) { body.appendChild(row); });
  }
  document.addEventListener("click", function (event) {
    var button = event.target.closest("[data-sort]");
    if (!button) return;
    var key = button.getAttribute("data-sort");
    var direction = button.getAttribute("data-direction") === "asc" ? "desc" : "asc";
    Array.prototype.forEach.call(document.querySelectorAll("[data-sort]"), function (item) {
      item.removeAttribute("data-direction");
    });
    button.setAttribute("data-direction", direction);
    sortRows(key, direction);
  });

  window.addLandscapeColumn = function (purl) {
    var table = document.querySelector("table.landscape");
    if (!table || !data.membership) return;
    var header = table.tHead.rows[0];
    var label = (data.labels && data.labels[purl]) || purl;
    var th = document.createElement("th");
    th.className = "rotate";
    th.textContent = label;
    header.appendChild(th);
    var membership = data.membership[purl] || {};
    Array.prototype.forEach.call(table.tBodies[0].rows, function (row) {
      var repo = row.getAttribute("data-repo");
      var cell = document.createElement("td");
      cell.className = "cell";
      if ((membership.direct || []).indexOf(repo) >= 0) {
        cell.className = "cell direct";
        cell.title = "direct";
      } else if ((membership.transitive || []).indexOf(repo) >= 0) {
        cell.className = "cell transitive";
        cell.title = "transitive";
      }
      row.appendChild(cell);
    });
  };
  document.addEventListener("click", function (event) {
    var button = event.target.closest("[data-add-column]");
    if (!button) return;
    window.addLandscapeColumn(button.getAttribute("data-add-column"));
  });
})();
