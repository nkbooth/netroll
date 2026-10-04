/*
 * VENDORED FROM Material for MkDocs 9.7.7 —
 * material/templates/partials/javascripts/base.html
 * (image digest sha256:51b87149d227691486b5f08993d28c65ca7e4990010664b697265b8e6fcd5287,
 * pinned at Containerfile:67).
 *
 * Upstream inlines this in the page. Production's CSP allows scripts by
 * `script-src 'self'` only, so an inline block is refused — and this block
 * defines __md_get, which Material's bundle calls during startup. Refusing it
 * kills the entire bundle, and with it the code that sizes the primary
 * sidebar's scroll container, which is why the nav could not be scrolled to
 * its lower entries on the live site. Served from a file, it
 * loads as 'self' and the policy needs no exception.
 *
 * The declarations are deliberately implicit globals, exactly as upstream
 * writes them: the bundle reads them off `window`.
 *
 * The ONLY edit to upstream's body is the scope. Upstream interpolates it
 * into the source with Jinja, which makes the script text differ per page
 * depth; here it is read from the script tag's `data-md-scope`, set by
 * overrides/partials/javascripts/base.html to the same expression upstream
 * uses. Same value, same per-page default, one file.
 *
 * ON A MATERIAL UPGRADE: re-diff against the new upstream partial.
 */
__md_scope=new URL(document.currentScript.getAttribute("data-md-scope"),location),__md_hash=e=>[...e].reduce(((e,_)=>(e<<5)-e+_.charCodeAt(0)),0),__md_get=(e,_=localStorage,t=__md_scope)=>JSON.parse(_.getItem(t.pathname+"."+e)),__md_set=(e,_,t=localStorage,a=__md_scope)=>{try{t.setItem(a.pathname+"."+e,JSON.stringify(_))}catch(e){}}
