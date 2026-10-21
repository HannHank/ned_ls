# ned_ls

<!--toc:start-->
- [ned_ls](#ned_ls)
  - [About](#about)
  - [Features](#features)
    - [NED](#ned)
    - [INI](#ini)
  - [Installation](#installation)
  - [Configure](#configure)
  - [Editors](#editors)
    - [Sublime-Text](#sublime-text)
      - [1. Install the Syntax Package](#1-install-the-syntax-package)
      - [2. Install the language server](#2-install-the-language-server)
      - [3. Goto Definition](#3-goto-definition)
    - [Neovim](#neovim)
    - [VS Code](#vs-code)
    - [Zed](#zed)
      - [Locating the binary](#locating-the-binary)
  - [License](#license)
<!--toc:end-->

> A simple language server for OMNeT++ simulation framework, supporting `ned` and `ini` files.

## About
ned_ls is an independent project and is not affiliated with OMNeT++ or OpenSim Ltd.
I started it in November 2025 to make my own OMNeT++ simulation work more pleasant,
and it grew feature by feature from there. This GitHub repository is the upstream
home of the project.

**Status:** I use it daily with OMNeT++ and INET. The core
features (goto definition, diagnostics, completion, rename) are stable for my
workflows, but expect rough edges in less common NED/INI constructs.

**Short Notes**:
- This is my first larger Rust project and my first language server. The original
  goal was only goto definition for module names, and parts of the architecture
  still reflect that.
- Moving to [Salsa](https://github.com/salsa-rs/salsa) made the server incremental
  and allowed most of the later features. Some older building blocks (like the
  Tree-sitter-based parsing) would be worth replacing at some point.
- I wrote the vast majority of this code myself and carefully reviewed the rest. Because I was short on time, I used AI to generate tests that mostly just cover the parsing and utility functions. I am planning a manual rewrite soon to improve this setup and increase coverage.
- The language server does not really know about different NED versions. In hindsight, I should have thought about that earlier. So far it only covers fairly basic constructs, so I have not run into any version problems, but I also have not tested it much.
## Features

### NED

- **Go to definition** for types, submodules, parameters and gates
- **Find reference** and **rename** (types, with checks against invalid or colliding names)
- **Hover** shows the documentation comment of the referenced type
- **Completion** for keywords, types, submodules, parameters, gates, connections, and context-aware `package` / `import` statements (the context is not always perfectly detected because I am not covering all edge cases yet)
- **Formatting** → very basic
- **Diagnostics**
    - syntax errors
    - package declarations that do not match the folder structure
    - unresolved types, imports, `extends` and `like` targets
    - inheritance rules (cyclic `extends`)
    - interface conformance (missing gates/parameters, direction or vector mismatches → some missing edge cases)
    - connections (unknown gate, wrong direction, missing vector index, basic $i/$o misuse detection)
    - duplicate submodules and parameters used before they are declared

### INI
- **Completion** for configuration options, parameter keys of the selected network, and extends
- **Go to definition** from INI keys and network names to the NED source → very helpful 
- **Diagnostics** (intentionally strict, so expect occasional false positives for things that are later resolved)
    - unknown networks
    - assignments to parameters/gates that do not exist
    - wrong value types for built-in options
    - duplicates keys
    - circular `extends` between sections

## Installation

VS Code users can skip this: the extension ships with the language server (see [VS Code](#vs-code)).

For all other editors, either download a prebuilt binary from the
[Releases page](https://github.com/HannHank/ned_ls/releases) or build it from source, which requires a Rust toolchain and a C compiler (because of Tree-sitter): 

```bash
cargo install --git https://github.com/HannHank/ned_ls
```

## Configure

To index related projects, you can create a Ned_ls.toml file in the root of your workspace with the following content:

```toml
imported_projects = [
	"/<full-path-to-project>/inet/",
	"/<full-path-to-project>/AutoTSN/",
	"~/AutoTSN/",
	"../AutoTSN/",
]
```

## Editors 

> All Editors supporting the *Language Server Protocol* 

- Acme
- Emacs
- Helix
- Lapce
- [Sublime-Text](#sublime-text)
- [VS Code](#vs-code)
- [Vim or Neovim](#neovim)
- [Zed](#zed)

### Sublime-Text

> [!NOTE]
> Assuming `ned_ls` is installed (see [Installation](#installation)). 

#### 1. Install the Syntax Package

*TODO: Remove manual steps once the package is on Package Control.*

Until the package is published, you must install the syntax highlighting manually:
1. In Sublime Text, go to `Preferences > Browse Packages...` to open your `Packages` directory.
2. Open the `User` folder.
3. Copy the contents of the `editors/sublime` folder from the `ned_ls` folder into this `User` folder.

#### 2. Install the language server 

1. Install the **LSP** package: `Package Control: Install Package` → `LSP`.
2. Open `Preferences > Package Settings > LSP > Settings` and add:

```json
{
    "clients": {
        "ned_ls": {
            "enabled": true,
            "command": ["ned_ls"], // or full path if binary is not globally available 
            "selector": "source.ned | source.ini.omnetpp"
        }
    }
}
```

#### 3. Goto Definition

I have not used Sublime Text a lot and there are reasons for it. For a smoother experience, I recommend creating a new mapping for LSP goto definitions: 

Go to `Preferences > Browse Packages...` and open the **`User`** folder. 

Inside that folder, create the following file based on your system: 

    Linux: Default (Linux).sublime-mousemap
    Windows: Default (Windows).sublime-mousemap
    Mac: Default (OSX).sublime-mousemap

Paste the following into the file and save:

```json
[
    {
        "button": "button1", 
        "count": 1, 
        "modifiers": ["ctrl"],
        "press_command": "drag_select",
        "command": "lsp_symbol_definition"
    }
]
```

### Neovim

Quick configuration, thanks to the native LSP-Client. 
```lua 
-- Neovim does not know .ned files by default
vim.filetype.add({ extension = { ned = "ned" } })

local capabilities = vim.lsp.protocol.make_client_capabilities()
capabilities.workspace.didChangeWatchedFiles.dynamicRegistration = true -- enable file watching

vim.api.nvim_create_autocmd("FileType", {
	pattern = {"ned", "dosini"}, -- Neovim uses the filetype "dosini" for .ini files
	callback = function()
		-- vim.lsp.set_log_level("info")
		vim.lsp.start({
			name = "ned_ls",
			cmd = { "<path-to-ls>/ned_ls" },
			capabilities = capabilities,
			-- capabilities = require("cmp_nvim_lsp").default_capabilities(), might be used in the future for more complex completion support
			root_dir = vim.loop.cwd(),
			filetypes = { "ned", "dosini" },
		})
	end,
})
```

**VIM**

Since VIM does not offer a native LSP-Client implementation like Neovim, we need to install it manually:

```vim
Plug 'prabirshrestha/vim-lsp'
```

Now we can tell vim to start the language server on `.ned` and `.ini` files (Vim uses the filetype `dosini` for `.ini` files):
```vim
if executable('ned_ls')
  augroup NedLSP
    autocmd!
    autocmd User lsp_setup call lsp#register_server({
    \ 'name': 'ned_ls',
    \ 'cmd': {server_info->['ned_ls']},
    \ 'whitelist': ['ned', 'dosini'],
    \ })
  augroup END
endif
```
This requires that `ned_ls` is on your `$PATH`.

<details>
<summary>Example Configuration using Vundle</summary>

```vim
filetype plugin on
syntax on
set number
call vundle#begin()
" let Vundle manage Vundle, required
Plugin 'VundleVim/Vundle.vim'
Plugin 'prabirshrestha/vim-lsp'
call vundle#end()

augroup FiletypeNED
  autocmd!
  autocmd BufNewFile,BufRead *.ned setlocal filetype=ned
augroup END

let g:lsp_work_done_progress_enabled=0
let g:lsp_log_verbose = 0
" let g:lsp_log_verbose = 1
" let g:lsp_log_file = expand('~/vim-lsp.log')

if executable('ned_ls')
  augroup NedLSP
    autocmd!
    autocmd User lsp_setup call lsp#register_server({
    \ 'name': 'ned_ls',
	\ 'cmd': {server_info->['ned_ls', '--stdio']},
    \ 'whitelist': ['ned', 'dosini'],
    \ })
  augroup END
endif

function! s:on_lsp_buffer_enabled() abort
    setlocal omnifunc=lsp#complete
    setlocal signcolumn=yes
    if exists('+tagfunc') | setlocal tagfunc=lsp#tagfunc | endif
    nmap <buffer> gd <plug>(lsp-definition)
    nmap <buffer> gs <plug>(lsp-document-symbol-search)
    nmap <buffer> gS <plug>(lsp-workspace-symbol-search)
    nmap <buffer> gr <plug>(lsp-references)
    nmap <buffer> gi <plug>(lsp-implementation)
    nmap <buffer> gt <plug>(lsp-type-definition)
    nmap <buffer> <leader>rn <plug>(lsp-rename)
    nmap <buffer> [g <plug>(lsp-previous-diagnostic)
    nmap <buffer> ]g <plug>(lsp-next-diagnostic)
    nmap <buffer> K <plug>(lsp-hover)
    nnoremap <buffer> <expr><c-f> lsp#scroll(+4)
    nnoremap <buffer> <expr><c-d> lsp#scroll(-4)

    let g:lsp_format_sync_timeout = 1000
    autocmd! BufWritePre *.ned call execute('LspDocumentFormatSync')
    
    " refer to doc to add more commands
endfunction

augroup lsp_install
    au!
    autocmd User lsp_buffer_enabled call s:on_lsp_buffer_enabled()
augroup END
```

</details>

**Completion** can be added triggered via `<C-x><C-o>` if you have set the `omnifunc` as shown above. For a more advanced completion experience have a look at [asyncomplete](https://github.com/prabirshrestha/asyncomplete.vim) or other available completion plugins for vim. 

One important note: 

Since VIM does **not** support dynamic file watching registration, you might need to restart VIM after deleting files in order to update the index correctly.
This might change in the future if I implement an alternative file watching mechanism on the server side (Currently this is not prioritized).

### VS Code
Install the extension from the [Marketplace](https://marketplace.visualstudio.com/items?itemName=HannHank.ned-ls),
or search for "OMNeT++ NED & INI Language Server" in the Extensions view.

Alternatively, download the `.vsix` file from the [Releases page](https://github.com/HannHank/ned_ls/releases) and install it via the command line:

```bash
code --install-extension <path-to>.vsix
```

or in VS Code: open the Extensions view, click the ... menu on the top and choose Install from VSIX....

>[!Note]
>- `goto_definition` works via `F12` or `Ctrl+Click` on a symbol. On Linux you can go back using `Strg+Alt+-`. 

**Syntax Highlighting** via the [omnetpp-vscode-extension](https://github.com/omnetpp/omnetpp-vscode) is also possible in parallel. 

### Zed

- Install the official tree-sitter support for ned and msg files from omnetpp: [omnetpp-zed](https://github.com/omnetpp/omnetpp-zed) via the extension tab
- Now use the **Install Dev Extension** and select the *zed* folder of the **ned_ls** repository

#### Locating the binary

The extension looks for the server in this order:

1. An explicit path from your `settings.json` (see below).
2. `ned_ls` on your `PATH`.

So if `ned_ls` is on your `PATH` — for instance via `cargo install --path .`, which puts it in `~/.cargo/bin` — **no configuration is needed at all**. You only need to set a
path if the binary lives somewhere unusual, or if you want to run a specific build:

```json
{
  "lsp": {
    "ned_ls": {
      "binary": {
        "path": "/absolute/path/to/target/release/ned_ls",
        "arguments": []
      }
    }
  }
}
```

>[!Note]
> Zed resolves the `PATH` from your login shell environment. If you launch Zed
> from a GUI launcher and your `PATH` is only extended in an interactive-shell
> rc file (`.bashrc` instead of `.profile`/`.zshenv`), the lookup can fail.
> Either move the `PATH` export to the login-shell file or set `binary.path`.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in ned_ls by you shall be dual licensed as above, without any
additional terms or conditions.
