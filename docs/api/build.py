# Builds docs/api/index.html: Swagger UI with every *-openapi.yaml in this folder embedded, so the page
# opens straight from disk (double-click) or from GitHub Pages, without a server.
#
#   python docs/api/build.py
#
# Edit the .yaml files, not index.html.
import glob, os

here = os.path.dirname(os.path.abspath(__file__))
specs = sorted(glob.glob(os.path.join(here, '*-openapi.yaml')))


def raw(path):
    # A script element's text is not HTML-decoded: kept as written, so it must never close the element.
    text = open(path, encoding='utf-8').read()
    assert '</script' not in text.lower(), path
    return text

blocks = '\n'.join(
    '<script type="text/plain" class="spec" data-name="%s">\n%s</script>' % (
        os.path.basename(p).replace('-openapi.yaml', ''), raw(p))
    for p in specs)

page = '''<!doctype html>
<html lang="id">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>aina Cloud API</title>
<link rel="stylesheet" href="https://cdnjs.cloudflare.com/ajax/libs/swagger-ui/5.17.14/swagger-ui.min.css">
<style>
  body { margin: 0; background: #fff; }
  .topbar-aina { display: flex; gap: 12px; align-items: center; padding: 12px 16px; background: #0b1220; color: #fff; font: 600 15px system-ui, sans-serif; }
  .topbar-aina select { font: inherit; padding: 4px 8px; border-radius: 6px; }
</style>
</head>
<body>
<div class="topbar-aina">aina Cloud API <select id="pick"></select></div>
<div id="ui"></div>
%s
<script src="https://cdnjs.cloudflare.com/ajax/libs/js-yaml/4.1.0/js-yaml.min.js"></script>
<script src="https://cdnjs.cloudflare.com/ajax/libs/swagger-ui/5.17.14/swagger-ui-bundle.min.js"></script>
<script>
  // A spec that doesn't parse says so on the page instead of leaving it blank.
  const specs = [];
  for (const s of document.querySelectorAll('script.spec')) {
    try {
      specs.push({ name: s.dataset.name, spec: jsyaml.load(s.textContent) });
    } catch (e) {
      const p = document.createElement('pre');
      p.style.cssText = 'margin:16px;padding:12px;background:#fee;color:#900;white-space:pre-wrap';
      p.textContent = s.dataset.name + '-openapi.yaml tidak bisa dibaca:\\n' + e.message;
      document.body.append(p);
    }
  }
  const pick = document.getElementById('pick');
  for (const s of specs) pick.append(new Option(s.spec.info.title, s.name));
  const show = (name) => SwaggerUIBundle({ spec: specs.find((s) => s.name === name).spec, dom_id: '#ui', deepLinking: true, docExpansion: 'list', defaultModelsExpandDepth: 0, tryItOutEnabled: false });
  pick.onchange = () => show(pick.value);
  show(specs[0].name);
</script>
</body>
</html>
''' % blocks

open(os.path.join(here, 'index.html'), 'w', encoding='utf-8', newline='\n').write(page)
print('docs/api/index.html:', ', '.join(os.path.basename(p) for p in specs))
