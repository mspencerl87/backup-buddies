// Shared option lists for the invite-creation and "Find a buddy" pages —
// a plain script (not a module) so both dashboard.html and
// find-a-buddy.html can load it with a single <script src=...> tag and
// use the globals below directly, no bundler involved.
//
// PLEDGE_GB_OPTIONS must track the API's PLEDGE_STEP_BYTES/PLEDGE_MAX_BYTES
// (apps/api/src/routes/invites.js) — the API is the real source of truth
// for what's accepted, this just has to keep matching it by hand.
// SPEED_TIER_OPTIONS' values must match what POST /invites validates.

const PLEDGE_GB_OPTIONS = [];
for (let gb = 5; gb <= 50; gb += 5) PLEDGE_GB_OPTIONS.push(gb);
for (let gb = 500; gb <= 20000; gb += 500) PLEDGE_GB_OPTIONS.push(gb);

function pledgeOptionLabel(gb) {
  if (gb === 50) return '50 GB (full free allowance)';
  return gb >= 1000 ? `${(gb / 1000).toFixed(gb % 1000 === 0 ? 0 : 1)} TB` : `${gb} GB`;
}

function populatePledgeSelect(selectEl, defaultGb) {
  selectEl.innerHTML = '';
  for (const gb of PLEDGE_GB_OPTIONS) {
    const option = document.createElement('option');
    option.value = String(gb);
    option.textContent = pledgeOptionLabel(gb);
    if (gb === defaultGb) option.selected = true;
    selectEl.appendChild(option);
  }
}

// Self-reported upload speed, bucketed rather than a raw number — a typed
// "Mbps" figure is rarely accurate (varies by time of day, ISP throttling,
// Wi-Fi vs. wired) and buckets are easier to scan on a listing anyway.
const SPEED_TIER_OPTIONS = [
  { value: 'under_10', label: 'Under 10 Mbps' },
  { value: '10_50', label: '10 – 50 Mbps' },
  { value: '50_200', label: '50 – 200 Mbps' },
  { value: '200_plus', label: '200 Mbps or more' },
];

function populateSpeedTierSelect(selectEl) {
  selectEl.innerHTML = '<option value="">Prefer not to say</option>';
  for (const { value, label } of SPEED_TIER_OPTIONS) {
    const option = document.createElement('option');
    option.value = value;
    option.textContent = label;
    selectEl.appendChild(option);
  }
}

function speedTierLabel(value) {
  return SPEED_TIER_OPTIONS.find((o) => o.value === value)?.label ?? null;
}

// Country only, deliberately — no state/city/postal code. This is self-
// reported on a *public* listing, and the API's own privacy stance for
// marketplace listings is "nothing here reveals who someone is" (see the
// comment on GET /invites/public); anything finer than country starts to
// narrow down where someone actually lives.
const COUNTRY_OPTIONS = [
  'United States', 'Canada', 'Mexico', 'United Kingdom', 'Ireland', 'France',
  'Germany', 'Netherlands', 'Belgium', 'Luxembourg', 'Switzerland', 'Austria',
  'Spain', 'Portugal', 'Italy', 'Greece', 'Poland', 'Czech Republic',
  'Slovakia', 'Hungary', 'Romania', 'Bulgaria', 'Croatia', 'Slovenia',
  'Serbia', 'Ukraine', 'Denmark', 'Sweden', 'Norway', 'Finland', 'Iceland',
  'Estonia', 'Latvia', 'Lithuania', 'Russia', 'Turkey', 'Israel',
  'United Arab Emirates', 'Saudi Arabia', 'South Africa', 'Nigeria', 'Egypt',
  'Kenya', 'Morocco', 'India', 'Pakistan', 'Bangladesh', 'China', 'Japan',
  'South Korea', 'Taiwan', 'Hong Kong', 'Singapore', 'Malaysia', 'Thailand',
  'Vietnam', 'Philippines', 'Indonesia', 'Australia', 'New Zealand',
  'Brazil', 'Argentina', 'Chile', 'Colombia', 'Peru', 'Uruguay',
  'Costa Rica', 'Other',
].sort((a, b) => (a === 'Other' ? 1 : b === 'Other' ? -1 : a.localeCompare(b)));

function populateCountrySelect(selectEl) {
  selectEl.innerHTML = '<option value="">Prefer not to say</option>';
  for (const country of COUNTRY_OPTIONS) {
    const option = document.createElement('option');
    option.value = country;
    option.textContent = country;
    selectEl.appendChild(option);
  }
}
