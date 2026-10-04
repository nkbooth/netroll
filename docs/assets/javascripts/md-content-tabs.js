/*
 * VENDORED VERBATIM FROM Material for MkDocs 9.7.7 —
 * material/templates/partials/javascripts/content.html (the
 * `content.tabs.link` branch)
 * (image digest sha256:51b87149d227691486b5f08993d28c65ca7e4990010664b697265b8e6fcd5287,
 * pinned at Containerfile:67).
 *
 * Externalised for the same reason as md-bootstrap.js: production's
 * `script-src 'self'` refuses inline blocks. Body unmodified.
 *
 * The `content.tabs.link` feature is not enabled in mkdocs.yml today, so this
 * file is not referenced by any built page. It is carried so that enabling
 * the feature later does not silently reintroduce an inline script.
 *
 * ON A MATERIAL UPGRADE: re-diff against the new upstream partial.
 */
var tabs=__md_get("__tabs");if(Array.isArray(tabs))e:for(var set of document.querySelectorAll(".tabbed-set")){var labels=set.querySelector(".tabbed-labels");for(var tab of tabs)for(var label of labels.getElementsByTagName("label"))if(label.innerText.trim()===tab){var input=document.getElementById(label.htmlFor);input.checked=!0;continue e}}
