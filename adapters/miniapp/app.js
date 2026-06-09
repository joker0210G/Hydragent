// Initialize Telegram WebApp SDK
const tg = window.Telegram?.WebApp;
if (tg) {
    tg.ready();
    tg.expand();
    document.body.classList.add('telegram-themed');
    tg.setHeaderColor('secondary_bg_color');
}

// Global state variables
let socket = null;
let shelvesList = [];
let booksList = [];
let pagesList = [];
let memoriesList = [];
let activePageId = '';
let currentLinkType = 'page_to_book';

// Helper to generate UUIDs
function generateUUID() {
    return 'xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx'.replace(/[xy]/g, function(c) {
        var r = Math.random() * 16 | 0, v = c == 'x' ? r : (r & 0x3 | 0x8);
        return v.toString(16);
    });
}

// WebSocket initialization
function connectWebSocket() {
    const wsProto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    let queryParams = "";
    if (window.Telegram && window.Telegram.WebApp && window.Telegram.WebApp.initData) {
        queryParams = `?initData=${encodeURIComponent(window.Telegram.WebApp.initData)}`;
    } else if (window.location.search) {
        queryParams = window.location.search;
    }
    const wsUrl = `${wsProto}//${location.host}/ws${queryParams}`;
    
    socket = new WebSocket(wsUrl);
    
    socket.onopen = () => {
        logger("Connected to core adapter.");
        fetchData();
    };
    
    socket.onmessage = (event) => {
        try {
            const data = JSON.parse(event.data);
            handleIncomingMessage(data);
        } catch (e) {
            console.error("Failed to parse websocket message:", e);
        }
    };
    
    socket.onerror = (error) => {
        console.error("Websocket error:", error);
    };
    
    socket.onclose = () => {
        logger("Connection lost. Reconnecting in 3 seconds...");
        setTimeout(connectWebSocket, 3000);
    };
}

function logger(message, type = 'system') {
    const consoleBox = document.getElementById('console-logs');
    if (!consoleBox) return;
    const line = document.createElement('div');
    line.className = `log-line ${type}`;
    const timeStr = new Date().toLocaleTimeString();
    line.innerText = `[${timeStr}] ${message}`;
    consoleBox.appendChild(line);
    consoleBox.scrollTop = consoleBox.scrollHeight;
}

// Tabs switching
function switchTab(tabId) {
    document.querySelectorAll('.tab-btn').forEach(btn => btn.classList.remove('active'));
    document.querySelectorAll('.tab-content').forEach(content => content.classList.remove('active'));
    
    const selectedBtn = Array.from(document.querySelectorAll('.tab-btn')).find(b => b.innerText.toLowerCase().includes(tabId));
    if (selectedBtn) selectedBtn.classList.add('active');
    
    const selectedContent = document.getElementById(`${tabId}-tab`);
    if (selectedContent) selectedContent.classList.add('active');

    // Automatically reload graph iframe when switching to the graph tab to show fresh data
    if (tabId === 'graph') {
        const graphIframe = document.querySelector('#graph-tab iframe');
        if (graphIframe) {
            graphIframe.src = 'graph.html?t=' + Date.now();
        }
    }
}

// Request lists from server
function fetchData() {
    if (!socket || socket.readyState !== WebSocket.OPEN) return;
    
    // Fetch Shelves
    socket.send(JSON.stringify({
        jsonrpc: "2.0",
        method: "library.list_nodes",
        params: { type: "shelf" },
        id: "get_shelves"
    }));
    
    // Fetch Books
    socket.send(JSON.stringify({
        jsonrpc: "2.0",
        method: "library.list_nodes",
        params: { type: "book" },
        id: "get_books"
    }));
    
    // Fetch Pages and Active Page info
    socket.send(JSON.stringify({
        jsonrpc: "2.0",
        method: "room.list",
        id: "get_pages"
    }));
    
    // Fetch Memories List
    socket.send(JSON.stringify({
        jsonrpc: "2.0",
        method: "memory.list",
        id: "get_memories"
    }));
}

// Toast notification handler
function showToast(message, type = 'success') {
    const container = document.getElementById('toast-container');
    if (!container) return;
    
    const toast = document.createElement('div');
    toast.className = 'toast';
    
    let emoji = '✅';
    if (type === 'error') emoji = '❌';
    else if (type === 'info') emoji = 'ℹ️';
    
    toast.innerHTML = `<span>${emoji}</span><span>${message}</span>`;
    container.appendChild(toast);
    
    setTimeout(() => {
        toast.remove();
    }, 3000);
}

// Handle responses
function handleIncomingMessage(msg) {
    if (msg.error) {
        showToast(msg.error.message || "An error occurred", 'error');
        return;
    }

    if (msg.method === "response.status") {
        logger(msg.params.status, 'step');
        return;
    }
    if (msg.method === "response.token") {
        logger(`Token output: ${msg.params.token}`, 'system');
        return;
    }
    if (msg.method === "gateway.push") {
        logger(`Push notification: ${msg.params.content}`, 'system');
        return;
    }

    if (msg.id === "get_shelves") {
        shelvesList = msg.result || [];
        renderShelves();
        updateLinkDropdowns();
    } else if (msg.id === "get_books") {
        booksList = msg.result || [];
        renderBooks();
        updateLinkDropdowns();
    } else if (msg.id === "get_pages") {
        pagesList = msg.result ? (msg.result.rooms || []) : [];
        activePageId = msg.result ? (msg.result.active_room || '') : '';
        renderPages();
        updateLinkDropdowns();
    } else if (msg.id === "get_memories" && msg.result) {
        memoriesList = msg.result || [];
        renderMemories();
    } else if (msg.result && (msg.result.status === "created" || msg.result.status === "linked" || msg.result.status === "deleted" || msg.result.status === "renamed")) {
        fetchData();
        showToast(`Item successfully ${msg.result.status}!`, 'success');
        const graphIframe = document.querySelector('#graph-tab iframe');
        if (graphIframe) {
            graphIframe.src = 'graph.html?t=' + Date.now();
        }
    }
}

// Render Shelves
function renderShelves() {
    const container = document.getElementById('shelves-list');
    if (!container) return;
    container.innerHTML = '';
    
    if (shelvesList.length === 0) {
        container.innerHTML = '<div class="loader">No shelves found.</div>';
        return;
    }
    
    shelvesList.forEach(shelf => {
        const item = document.createElement('div');
        item.className = 'room-item';
        item.innerHTML = `
            <div class="room-info">
                <span class="room-title">📚 ${shelf.label}</span>
                <span class="room-id">Shelf ID: ${shelf.id.substring(0, 8)}</span>
            </div>
            <div class="room-actions">
                <button class="btn-icon" onclick="event.stopPropagation(); openRenameModal('${shelf.id}', '${shelf.label}')">✏️</button>
                <button class="btn-icon" onclick="event.stopPropagation(); deleteNode('${shelf.id}')">🗑️</button>
            </div>
        `;
        container.appendChild(item);
    });
}

// Render Books
function renderBooks() {
    const container = document.getElementById('books-list');
    if (!container) return;
    container.innerHTML = '';
    
    if (booksList.length === 0) {
        container.innerHTML = '<div class="loader">No books found.</div>';
        return;
    }
    
    booksList.forEach(book => {
        const item = document.createElement('div');
        item.className = 'room-item';
        item.innerHTML = `
            <div class="room-info">
                <span class="room-title">📘 ${book.label}</span>
                <span class="room-id">Book ID: ${book.id.substring(0, 8)}</span>
            </div>
            <div class="room-actions">
                <button class="btn-icon" onclick="event.stopPropagation(); openRenameModal('${book.id}', '${book.label}')">✏️</button>
                <button class="btn-icon" onclick="event.stopPropagation(); deleteNode('${book.id}')">🗑️</button>
            </div>
        `;
        container.appendChild(item);
    });
}

// Render Pages
function renderPages() {
    const container = document.getElementById('pages-list');
    if (!container) return;
    container.innerHTML = '';
    
    if (pagesList.length === 0) {
        container.innerHTML = '<div class="loader">No pages found.</div>';
        return;
    }
    
    // Auto-resolve activePageId if it is empty or does not exist in the loaded list
    if (!activePageId || !pagesList.some(p => p.id === activePageId)) {
        activePageId = pagesList[0].id;
    }
    
    pagesList.forEach(page => {
        const isActive = page.id === activePageId;
        const item = document.createElement('div');
        item.className = `room-item ${isActive ? 'active' : ''}`;
        item.onclick = () => selectPage(page.id, page.label);
        
        item.innerHTML = `
            <div class="room-info">
                <span class="room-title">📄 ${page.label}</span>
                <span class="room-id">Page ID: ${page.id.substring(0, 8)}</span>
            </div>
            <div class="room-actions">
                <button class="btn-icon" onclick="event.stopPropagation(); openRenameModal('${page.id}', '${page.label}')">✏️</button>
                <button class="btn-icon" onclick="event.stopPropagation(); deleteNode('${page.id}')">🗑️</button>
            </div>
        `;
        container.appendChild(item);
        
        if (isActive) {
            const display = document.getElementById('header-active-room');
            if (display) display.innerText = page.label;
        }
    });
}

// Select Page (session context)
function selectPage(pageId, pageLabel) {
    if (pageId === activePageId) {
        // If already active, just close the webapp to return to chat
        if (tg) tg.close();
        return;
    }
    
    showConfirmation(`Switch conversation context to "${pageLabel}" and return to chat?`, () => {
        activePageId = pageId;
        renderPages();
        if (!socket || socket.readyState !== WebSocket.OPEN) return;
        socket.send(JSON.stringify({
            jsonrpc: "2.0",
            method: "room.switch",
            params: { room_id: pageId },
            id: "switch_page"
        }));
        if (tg) {
            tg.close();
        }
    });
}

// Set link type for guided connection
function setLinkType(type) {
    currentLinkType = type;
    
    // Toggle active classes on connection buttons
    document.querySelectorAll('.link-type-btn').forEach(btn => btn.classList.remove('active'));
    
    const activeBtnId = type === 'page_to_book' ? 'btn-link-p2b' :
                        type === 'book_to_shelf' ? 'btn-link-b2s' : 'btn-link-s2s';
    const activeBtn = document.getElementById(activeBtnId);
    if (activeBtn) activeBtn.classList.add('active');
    
    // Update visual relationship display label
    const relationLabel = document.getElementById('link-relation-label');
    if (relationLabel) {
        relationLabel.innerText = type === 'page_to_book' ? 'belongs_to' :
                                 type === 'book_to_shelf' ? 'sits_on' : 'connects_to';
    }
    
    // Populate the dropdown options with filtered nodes
    updateLinkDropdowns();
}

// Update Link Dropdowns with strict type filtering
function updateLinkDropdowns() {
    const sourceSelect = document.getElementById('link-source');
    const targetSelect = document.getElementById('link-target');
    if (!sourceSelect || !targetSelect) return;
    
    sourceSelect.innerHTML = '<option value="">Select Source...</option>';
    targetSelect.innerHTML = '<option value="">Select Target...</option>';
    
    if (currentLinkType === 'page_to_book') {
        pagesList.forEach(p => sourceSelect.innerHTML += `<option value="${p.id}">📄 Page: ${p.label}</option>`);
        booksList.forEach(b => targetSelect.innerHTML += `<option value="${b.id}">📘 Book: ${b.label}</option>`);
    } else if (currentLinkType === 'book_to_shelf') {
        booksList.forEach(b => sourceSelect.innerHTML += `<option value="${b.id}">📘 Book: ${b.label}</option>`);
        shelvesList.forEach(s => targetSelect.innerHTML += `<option value="${s.id}">📚 Shelf: ${s.label}</option>`);
    } else if (currentLinkType === 'shelf_to_shelf') {
        shelvesList.forEach(s => sourceSelect.innerHTML += `<option value="${s.id}">📚 Shelf: ${s.label}</option>`);
        shelvesList.forEach(s => targetSelect.innerHTML += `<option value="${s.id}">📚 Shelf: ${s.label}</option>`);
    }
}

// Actions
function createNode(type) {
    const input = document.getElementById(`new-${type}-title`);
    if (!input) return;
    const label = input.value.trim();
    if (!label) return;
    
    const id = generateUUID();
    socket.send(JSON.stringify({
        jsonrpc: "2.0",
        method: "library.create_node",
        params: {
            id: id,
            type: type,
            label: label,
            properties: JSON.stringify({ created_at: Date.now() })
        },
        id: "create_node"
    }));
    
    input.value = '';
}

function linkNodes() {
    const source = document.getElementById('link-source').value;
    const target = document.getElementById('link-target').value;
    
    if (!source || !target) {
        showToast("Please select both a source and target item.", 'error');
        return;
    }
    
    const relation = currentLinkType === 'page_to_book' ? 'belongs_to' :
                     currentLinkType === 'book_to_shelf' ? 'sits_on' : 'connects_to';
    
    socket.send(JSON.stringify({
        jsonrpc: "2.0",
        method: "library.link",
        params: {
            source: source,
            relation: relation,
            target: target,
            weight: 1.0
        },
        id: "link_nodes"
    }));
}

function deleteNode(nodeId) {
    showConfirmation("Are you sure you want to delete this library item? All connections to it will be lost.", () => {
        socket.send(JSON.stringify({
            jsonrpc: "2.0",
            method: "library.delete_node",
            params: { id: nodeId },
            id: "delete_node"
        }));
    });
}

function showConfirmation(message, onConfirm) {
    if (tg && typeof tg.showConfirm === 'function') {
        tg.showConfirm(message, (approved) => {
            if (approved) onConfirm();
        });
    } else {
        if (confirm(message)) onConfirm();
    }
}

// Rename Modal
function openRenameModal(id, title) {
    document.getElementById('rename-item-id').value = id;
    document.getElementById('rename-item-input').value = title;
    document.getElementById('rename-modal').classList.add('open');
}

function closeRenameModal() {
    document.getElementById('rename-modal').classList.remove('open');
}

function submitRenameItem() {
    const id = document.getElementById('rename-item-id').value;
    const title = document.getElementById('rename-item-input').value.trim();
    if (!title) return;
    
    let type = '';
    if (shelvesList.some(s => s.id === id)) type = 'shelf';
    else if (booksList.some(b => b.id === id)) type = 'book';
    else if (pagesList.some(p => p.id === id)) type = 'page';
    
    if (type) {
        socket.send(JSON.stringify({
            jsonrpc: "2.0",
            method: "library.create_node",
            params: {
                id: id,
                type: type,
                label: title
            },
            id: "rename_node"
        }));
    }
    closeRenameModal();
}

// Render Memories
function renderMemories() {
    const listContainer = document.getElementById('memories-list');
    if (!listContainer) return;
    listContainer.innerHTML = '';
    
    if (memoriesList.length === 0) {
        listContainer.innerHTML = '<div class="loader">Memory database is empty.</div>';
        return;
    }
    
    memoriesList.forEach(mem => {
        const item = document.createElement('div');
        item.className = 'memory-item';
        item.setAttribute('data-content', mem.content.toLowerCase());
        
        const dateStr = new Date(mem.timestamp).toLocaleString();
        const importanceClass = mem.importance >= 4 ? 'high' : '';
        
        item.innerHTML = `
            <div class="memory-content">${mem.content}</div>
            <div class="memory-meta">
                <span class="importance-badge ${importanceClass}">Importance: ${mem.importance}</span>
                <span>${dateStr}</span>
                <button class="btn-delete-mem" onclick="deleteMemory('${mem.id}')">🗑️ Forget</button>
            </div>
        `;
        listContainer.appendChild(item);
    });
}

function filterMemories() {
    const query = document.getElementById('memory-search').value.toLowerCase();
    document.querySelectorAll('.memory-item').forEach(item => {
        const content = item.getAttribute('data-content');
        if (content.includes(query)) {
            item.style.display = 'flex';
        } else {
            item.style.display = 'none';
        }
    });
}

function deleteMemory(memId) {
    showConfirmation("Forget this memory? Hydragent will no longer remember this fact.", () => {
        socket.send(JSON.stringify({
            jsonrpc: "2.0",
            method: "memory.delete",
            params: { id: memId },
            id: "delete_memory"
        }));
    });
}

function confirmClearAllMemories() {
    showConfirmation("⚠️ WARNING: This will clear ALL stored memories. Irreversible. Proceed?", () => {
        socket.send(JSON.stringify({
            jsonrpc: "2.0",
            method: "memory.clear",
            id: "clear_memories"
        }));
    });
}

// Autocomplete @ references logic
let activeInput = null;
let currentAutocompleteQueryStart = -1;

function setupAutocomplete() {
    const popup = document.getElementById('autocomplete-popup');
    
    document.addEventListener('input', (e) => {
        const input = e.target;
        if (input.tagName !== 'INPUT' || input.type !== 'text') {
            popup.style.display = 'none';
            return;
        }
        
        activeInput = input;
        const cursor = input.selectionStart;
        const val = input.value;
        const textBefore = val.substring(0, cursor);
        const match = textBefore.match(/@(\w*)$/);
        
        if (match) {
            currentAutocompleteQueryStart = match.index;
            const query = match[1].toLowerCase();
            showAutocompletePopup(input, query);
        } else {
            popup.style.display = 'none';
        }
    });
    
    // Close popup on click outside
    document.addEventListener('click', (e) => {
        if (!popup.contains(e.target) && e.target !== activeInput) {
            popup.style.display = 'none';
        }
    });
}

function showAutocompletePopup(input, query) {
    const popup = document.getElementById('autocomplete-popup');
    popup.innerHTML = '';
    
    // Collect all nodes
    const allNodes = [
        ...shelvesList.map(n => ({ ...n, icon: '📚', typeLabel: 'Shelf' })),
        ...booksList.map(n => ({ ...n, icon: '📘', typeLabel: 'Book' })),
        ...pagesList.map(n => ({ ...n, icon: '📄', typeLabel: 'Page' }))
    ];
    
    const filtered = allNodes.filter(n => n.label.toLowerCase().includes(query));
    
    if (filtered.length === 0) {
        popup.style.display = 'none';
        return;
    }
    
    filtered.forEach(node => {
        const item = document.createElement('div');
        item.className = 'autocomplete-item';
        item.innerHTML = `
            <span class="autocomplete-icon">${node.icon}</span>
            <span>${node.label}</span>
            <span style="font-size:10px; color:var(--hint-color); margin-left:auto;">${node.typeLabel}</span>
        `;
        item.onclick = () => selectAutocompleteItem(node);
        popup.appendChild(item);
    });
    
    // Position popup below input
    const rect = input.getBoundingClientRect();
    popup.style.top = `${rect.bottom + window.scrollY + 5}px`;
    popup.style.left = `${rect.left + window.scrollX}px`;
    popup.style.width = `${rect.width}px`;
    popup.style.display = 'flex';
}

function selectAutocompleteItem(node) {
    if (!activeInput) return;
    const popup = document.getElementById('autocomplete-popup');
    
    const val = activeInput.value;
    const cursor = activeInput.selectionStart;
    
    const beforeQuery = val.substring(0, currentAutocompleteQueryStart);
    const afterCursor = val.substring(cursor);
    
    // Insert node reference tag
    const referenceTag = `@${node.label} `;
    activeInput.value = beforeQuery + referenceTag + afterCursor;
    
    // Refocus and place cursor after inserted text
    activeInput.focus();
    const newCursorPos = currentAutocompleteQueryStart + referenceTag.length;
    activeInput.setSelectionRange(newCursorPos, newCursorPos);
    
    popup.style.display = 'none';
}

// Initial connection and setup
connectWebSocket();
setupAutocomplete();
