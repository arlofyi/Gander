// SPDX-License-Identifier: Apache-2.0
//! The Windows Search index (WP-66): is a folder in its crawl scope, how far behind is it, and
//! run one SQL statement against it (`gander_core::search` builds the statement).
//!
//! # How it talks to the indexer
//!
//! - `CSearchManager` (a local COM server, `SearchIndexer.exe`) gives the `SystemIndex` catalogue;
//!   its [`ISearchCrawlScopeManager`] answers "is this folder crawled" and its
//!   [`ISearchQueryHelper`] supplies the provider's connection string (so a change of provider
//!   version is the OS's business, not ours).
//! - Statements run through the OLE DB provider `Search.CollatorDSO`, opened with
//!   `IDataInitialize::GetDataSource` and read with `IRowset`: forward-only, one string column
//!   bound by reference to provider-owned memory that is copied before the rows are released.
//! - Everything for one call happens on the calling thread inside a [`ComScope`], which joins the
//!   thread to the process MTA and keeps the MTA alive (see `com.rs` for the crash that
//!   motivated it). The COM objects are dropped BEFORE the scope (field order below), and an
//!   [`IndexSession`] is not `Send`.
//!
//! Nothing here changes the indexer: no crawl scope is added, no setting touched, nothing is
//! written. A stopped Windows Search service, a missing catalogue or any COM failure is an
//! [`IndexError`], and the caller answers from the bounded walk instead.

use std::ops::ControlFlow;

use gander_core::search::IndexScope;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER, CoCreateInstance, CoTaskMemFree,
};
use windows::Win32::System::Search::{
    CSearchManager, DBBINDING, DBMEMOWNER_PROVIDEROWNED, DBPARAMIO_NOTPARAM, DBPART_STATUS,
    DBPART_VALUE, DBSTATUS_S_OK, DBTYPE_BYREF, DBTYPE_WSTR, HACCESSOR, IAccessor, ICommand,
    ICommandText, IDBCreateCommand, IDBCreateSession, IDBInitialize, IDataInitialize, IRowset,
    ISearchCatalogManager, ISearchCrawlScopeManager, ISearchManager, MSDAINITIALIZE,
};
use windows::core::{GUID, HSTRING, Interface, PCWSTR, w};

use crate::com::ComScope;

/// `DBGUID_DEFAULT`: the provider's default command dialect.
const DBGUID_DEFAULT: GUID = GUID::from_u128(0xc8b521fb_5cf3_11ce_ade5_00aa0044773d);

/// `DBACCESSOR_ROWDATA`.
const DBACCESSOR_ROWDATA: u32 = 2;

/// Rows fetched per `GetNextRows` call.
const BATCH: usize = 64;

/// A failure talking to the indexer or the provider. The text names the step and the HRESULT.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Windows Search: {stage} failed: {detail}")]
pub struct IndexError {
    /// The step that failed (`"open catalogue"`, `"execute"`, ...).
    pub stage: &'static str,
    /// The HRESULT and the system's message.
    pub detail: String,
}

fn fail(stage: &'static str) -> impl FnOnce(windows::core::Error) -> IndexError {
    move |e| IndexError {
        stage,
        detail: format!("{e}"),
    }
}

/// How far behind the indexer is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Backlog {
    /// Items queued for incremental indexing.
    pub incremental: i32,
    /// Items queued from change notifications.
    pub notifications: i32,
    /// Items queued at high priority.
    pub high_priority: i32,
}

impl Backlog {
    /// Whether the indexer is still working through items it has to crawl, so results may miss
    /// files it has not reached yet. Only the incremental and high-priority queues count: the
    /// notification queue can sit above zero for good (measured 2026-11-03: a constant 114 for
    /// minutes on an idle machine, items the indexer cannot process), which would mark every
    /// answer partial forever.
    pub fn is_catching_up(&self) -> bool {
        self.incremental > 0 || self.high_priority > 0
    }
}

/// How a query ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryEnd {
    /// Rows consumed after the skipped ones: every row the visitor saw and let pass, plus rows
    /// with no usable value (NULL, not valid UTF-16), which are consumed without a visit. The
    /// row the visitor refused is NOT counted, so `skip + consumed` is where the next read starts.
    pub consumed: u64,
    /// The visitor refused a row, so the rowset has at least one more.
    pub stopped: bool,
}

/// The one row buffer: the column's status and a pointer to provider-owned UTF-16.
#[repr(C)]
struct RowBuf {
    status: u32,
    value: *const u16,
}

/// An open connection to the `SystemIndex` catalogue. Not `Send`.
pub struct IndexSession {
    catalog: ISearchCatalogManager,
    scope: ISearchCrawlScopeManager,
    connection: HSTRING,
    // Last field: dropped after every COM object above.
    _com: ComScope,
}

impl std::fmt::Debug for IndexSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexSession").finish_non_exhaustive()
    }
}

impl IndexSession {
    /// Connects to the indexer service. Fails when Windows Search is stopped or disabled.
    pub fn open() -> Result<Self, IndexError> {
        let com = ComScope::enter();
        // SAFETY: COM is initialised on this thread (`com`); the class id and interface are the
        // documented ones and every out-pointer is owned by the windows crate's wrappers.
        let (catalog, scope, connection) = unsafe {
            let manager: ISearchManager =
                CoCreateInstance(&CSearchManager, None, CLSCTX_LOCAL_SERVER)
                    .map_err(fail("connect to the indexer"))?;
            let catalog = manager
                .GetCatalog(w!("SystemIndex"))
                .map_err(fail("open the SystemIndex catalogue"))?;
            let helper = catalog
                .GetQueryHelper()
                .map_err(fail("get the query helper"))?;
            let raw = helper
                .ConnectionString()
                .map_err(fail("get the connection string"))?;
            let connection = HSTRING::from_wide(raw.as_wide());
            // The string is allocated by the callee with CoTaskMemAlloc.
            CoTaskMemFree(Some(raw.0 as *const std::ffi::c_void));
            let scope = catalog
                .GetCrawlScopeManager()
                .map_err(fail("get the crawl scope manager"))?;
            (catalog, scope, connection)
        };
        Ok(Self {
            catalog,
            scope,
            connection,
            _com: com,
        })
    }

    /// Whether the indexer crawls `dir` (an include rule covers it and no exclude rule overrides
    /// it). Read-only: the scope is never changed.
    pub fn is_in_scope(&self, dir: &IndexScope) -> Result<bool, IndexError> {
        let url = HSTRING::from(dir.crawl_url.as_str());
        // SAFETY: `url` is a valid NUL-terminated string for the call.
        let included = unsafe { self.scope.IncludedInCrawlScope(&url) }
            .map_err(fail("check the crawl scope"))?;
        Ok(included.as_bool())
    }

    /// The indexer's queue sizes; `None` when they cannot be read.
    pub fn backlog(&self) -> Option<Backlog> {
        let mut b = Backlog::default();
        // SAFETY: three valid out-pointers.
        unsafe {
            self.catalog.NumberOfItemsToIndex(
                &mut b.incremental,
                &mut b.notifications,
                &mut b.high_priority,
            )
        }
        .ok()
        .map(|()| b)
    }

    /// Runs `sql` and calls `visit` with the first column (a `System.ItemUrl`) of each row after
    /// the first `skip` rows. `visit` answers [`ControlFlow::Continue`] to take the row and go on,
    /// or [`ControlFlow::Break`] to leave that row unconsumed and stop (so the caller can tell
    /// "there is more" from "that was the last row"). Rows with no usable value are consumed
    /// without a visit.
    pub fn query(
        &self,
        sql: &str,
        skip: u64,
        visit: &mut dyn FnMut(&str) -> ControlFlow<()>,
    ) -> Result<QueryEnd, IndexError> {
        self.run(sql, skip, visit)
    }

    /// The query. COM is initialised on this thread (the session's `ComScope`); every pointer
    /// handed to the provider is to memory that outlives the call, the row buffer layout matches
    /// the binding, and the provider-owned string is copied before `ReleaseRows`.
    fn run(
        &self,
        sql: &str,
        skip: u64,
        visit: &mut dyn FnMut(&str) -> ControlFlow<()>,
    ) -> Result<QueryEnd, IndexError> {
        // The data source from the helper's connection string.
        // SAFETY: COM is initialised on this thread; the class id is the documented data
        // initializer and the interface is the one requested.
        let init: IDataInitialize =
            unsafe { CoCreateInstance(&MSDAINITIALIZE, None, CLSCTX_INPROC_SERVER) }
                .map_err(fail("create the data initializer"))?;
        let mut source = None;
        // SAFETY: `connection` is a valid NUL-terminated string, the IID is a static and `source`
        // is a valid out-pointer.
        unsafe {
            init.GetDataSource(
                None::<&windows::core::IUnknown>,
                CLSCTX_INPROC_SERVER.0,
                &self.connection,
                &IDBInitialize::IID,
                &mut source,
            )
        }
        .map_err(fail("create the data source"))?;
        let db: IDBInitialize = source
            .ok_or_else(|| IndexError {
                stage: "create the data source",
                detail: "no data source returned".into(),
            })?
            .cast()
            .map_err(fail("create the data source"))?;
        // SAFETY: a plain call on a live interface.
        unsafe { db.Initialize() }.map_err(fail("initialise the data source"))?;

        // Session and command.
        let create_session: IDBCreateSession = db.cast().map_err(fail("create a session"))?;
        // SAFETY: no outer object, a static IID.
        let session = unsafe {
            create_session.CreateSession(None::<&windows::core::IUnknown>, &IDBCreateCommand::IID)
        }
        .map_err(fail("create a session"))?;
        let create_command: IDBCreateCommand = session.cast().map_err(fail("create a command"))?;
        // SAFETY: no outer object, a static IID.
        let command = unsafe {
            create_command.CreateCommand(None::<&windows::core::IUnknown>, &ICommandText::IID)
        }
        .map_err(fail("create a command"))?;
        let text: ICommandText = command.cast().map_err(fail("create a command"))?;
        // SAFETY: a static GUID and a NUL-terminated string that lives through the call.
        unsafe { text.SetCommandText(&DBGUID_DEFAULT, &HSTRING::from(sql)) }
            .map_err(fail("set the command text"))?;
        let command: ICommand = text.cast().map_err(fail("create a command"))?;

        // Execute.
        let mut rowset = None;
        // SAFETY: no outer object, no parameters, a static IID and a valid out-pointer.
        unsafe {
            command.Execute(
                None::<&windows::core::IUnknown>,
                &IRowset::IID,
                None,
                None,
                Some(&mut rowset),
            )
        }
        .map_err(fail("execute"))?;
        let rowset: IRowset = rowset
            .ok_or_else(|| IndexError {
                stage: "execute",
                detail: "no rowset returned".into(),
            })?
            .cast()
            .map_err(fail("execute"))?;

        // One accessor: column 1 as a by-reference, provider-owned UTF-16 string plus status.
        let accessor: IAccessor = rowset.cast().map_err(fail("create an accessor"))?;
        let binding = DBBINDING {
            iOrdinal: 1,
            obValue: std::mem::offset_of!(RowBuf, value),
            obLength: 0,
            obStatus: std::mem::offset_of!(RowBuf, status),
            dwPart: (DBPART_VALUE.0 | DBPART_STATUS.0) as u32,
            dwMemOwner: DBMEMOWNER_PROVIDEROWNED.0 as u32,
            eParamIO: DBPARAMIO_NOTPARAM.0 as u32,
            wType: (DBTYPE_WSTR.0 | DBTYPE_BYREF.0) as u16,
            ..Default::default()
        };
        let mut haccessor = HACCESSOR::default();
        // SAFETY: one binding for column 1 whose offsets are those of `RowBuf`, the row size is
        // that of `RowBuf`, and `haccessor` is a valid out-pointer.
        unsafe {
            accessor.CreateAccessor(
                DBACCESSOR_ROWDATA,
                1,
                &binding,
                std::mem::size_of::<RowBuf>(),
                &mut haccessor,
                None,
            )
        }
        .map_err(fail("create an accessor"))?;
        // Released on every path out of this function.
        let _accessor_guard = AccessorGuard {
            accessor: &accessor,
            handle: haccessor,
        };

        let mut end = QueryEnd {
            consumed: 0,
            stopped: false,
        };
        let mut offset = isize::try_from(skip).unwrap_or(isize::MAX);
        let mut handles = [0usize; BATCH];
        loop {
            // `GetNextRows(HROW**)`: the binding passes `cRows = slice.len()` and the slice's own
            // address as `HROW**`, so the first element carries the address of the HROW array.
            let mut holder = [std::ptr::null_mut::<usize>(); BATCH];
            holder[0] = handles.as_mut_ptr();
            let mut got = 0usize;
            // SAFETY: `holder[0]` points to `BATCH` writable row handles (`handles`), `got` is a
            // valid out-pointer.
            unsafe { rowset.GetNextRows(0, offset, &mut got, &mut holder) }
                .map_err(fail("read rows"))?;
            offset = 0;
            if got == 0 {
                return Ok(end);
            }
            for &row in handles.iter().take(got) {
                let mut buf = RowBuf {
                    status: 0,
                    value: std::ptr::null(),
                };
                // SAFETY: `row` was returned by `GetNextRows`, `haccessor` is the accessor created
                // above and `buf` is a `RowBuf`, the layout it was bound to.
                let read =
                    unsafe { rowset.GetData(row, haccessor, std::ptr::addr_of_mut!(buf).cast()) };
                let url = match read {
                    Ok(()) if buf.status == DBSTATUS_S_OK.0 as u32 && !buf.value.is_null() => {
                        // SAFETY: the provider returned a NUL-terminated string that stays valid
                        // until the row is released; it is copied here.
                        unsafe { PCWSTR(buf.value).to_string() }.ok()
                    }
                    _ => None,
                };
                if let Some(url) = url
                    && visit(&url).is_break()
                {
                    end.stopped = true;
                    break;
                }
                end.consumed += 1;
            }
            // SAFETY: `handles[..got]` are the rows obtained above, each released once; the
            // optional out-arrays are null.
            unsafe {
                rowset.ReleaseRows(
                    got,
                    handles.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            }
            .map_err(fail("release rows"))?;
            if end.stopped {
                return Ok(end);
            }
        }
    }
}

/// Releases the accessor handle when the query ends.
struct AccessorGuard<'a> {
    accessor: &'a IAccessor,
    handle: HACCESSOR,
}

impl Drop for AccessorGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: the handle came from `CreateAccessor` on this accessor and is released once.
        let _ = unsafe { self.accessor.ReleaseAccessor(self.handle, None) };
    }
}
