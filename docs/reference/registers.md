---
title: Register index
description: Every XR819 register named in this manual, by address, with a filter.
---
Every register this manual names, by address, one row per word. Words only the MAC [receive image](mac.html#receive-register-image) or the PHY [loader](phy.html#register-writes) writes show the value written.

Filter by part of a name or address: `0e34`, `TSF` and `0x09c0_0e34` all work.

<p><input id="register-filter" class="filter" type="search" placeholder="Filter, for example GO or 0e8c" aria-label="Filter registers" autocomplete="off"> <span id="register-count" class="filter-count" aria-live="polite"></span></p>

<div class="table-wrap"><table class="registers" id="register-index">
<thead><tr><th style="text-align:right">Address</th><th>Register</th><th>Block</th></tr></thead>
<tbody>
{%- for r in site.data.registers %}
<tr><td style="text-align:right"><code>{{ r.address }}</code></td><td>{% if r.name contains '#' %}<a class="plain" href="{{ r.url }}">{{ r.name | markdownify | remove: '<p>' | remove: '</p>' | strip }}</a>{% else %}<a href="{{ r.url }}"><code>{{ r.name }}</code></a>{% endif %}</td><td>{{ r.block }}</td></tr>
{%- endfor %}
</tbody></table></div>

<script>
const filter = document.getElementById('register-filter');
const count = document.getElementById('register-count');
const rows = [...document.querySelectorAll('#register-index tbody tr')];
const flat = s => s.toLowerCase().replace(/_/g, '');
function apply() {
  const q = flat(filter.value.trim()).replace(/^0x/, '');
  let shown = 0;
  for (const r of rows) { r.hidden = q !== '' && !flat(r.textContent).includes(q); if (!r.hidden) shown++; }
  count.textContent = shown === rows.length ? `${rows.length} words` : shown ? `${shown} of ${rows.length}` : 'No match';
}
filter.addEventListener('input', apply);
apply();
</script>
