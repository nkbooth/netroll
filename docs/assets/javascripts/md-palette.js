/*
 * VENDORED VERBATIM FROM Material for MkDocs 9.7.7 —
 * material/templates/partials/javascripts/palette.html
 * (image digest sha256:51b87149d227691486b5f08993d28c65ca7e4990010664b697265b8e6fcd5287,
 * pinned at Containerfile:67).
 *
 * Externalised for the same reason as md-bootstrap.js: production's
 * `script-src 'self'` refuses inline blocks. This one restores the reader's
 * saved palette before first paint, so without it the dark/light choice does
 * not survive a page load. Body unmodified.
 *
 * ON A MATERIAL UPGRADE: re-diff against the new upstream partial.
 */
var palette=__md_get("__palette");if(palette&&palette.color){if("(prefers-color-scheme)"===palette.color.media){var media=matchMedia("(prefers-color-scheme: light)"),input=document.querySelector(media.matches?"[data-md-color-media='(prefers-color-scheme: light)']":"[data-md-color-media='(prefers-color-scheme: dark)']");palette.color.media=input.getAttribute("data-md-color-media"),palette.color.scheme=input.getAttribute("data-md-color-scheme"),palette.color.primary=input.getAttribute("data-md-color-primary"),palette.color.accent=input.getAttribute("data-md-color-accent")}for(var[key,value]of Object.entries(palette.color))document.body.setAttribute("data-md-color-"+key,value)}
