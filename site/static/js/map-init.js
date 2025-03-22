function initOSMMap() {
    const bboxData = document.getElementById('map').dataset.bbox;
    if (!bboxData) return;

    const bbox = JSON.parse(bboxData);
    const map = L.map('map').fitBounds([
        [bbox.minLat, bbox.minLon],
        [bbox.maxLat, bbox.maxLon]
    ]);

    L.tileLayer('https://{s}.tile.openstreetmap.org/{z}/{x}/{y}.png', {
        attribution: '© OpenStreetMap contributors'
    }).addTo(map);

    const bounds = [[bbox.minLat, bbox.minLon], [bbox.maxLat, bbox.maxLon]];
    const rectangle = L.rectangle(bounds, {
        color: '#d33838',
        weight: 2,
        fillOpacity: 0.1
    }).addTo(map);

    rectangle.bindPopup(`
        <b>Bounding Box:</b><br>
        SW: ${bbox.minLat.toFixed(5)}, ${bbox.minLon.toFixed(5)}<br>
        NE: ${bbox.maxLat.toFixed(5)}, ${bbox.maxLon.toFixed(5)}
    `);
}

// Initialize when Leaflet is loaded
if (typeof L !== 'undefined') {
    initOSMMap();
}
