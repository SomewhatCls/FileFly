# FileFly - a cross-platform, quad-panning file manager
<img width="1804" height="1328" alt="image" src="https://github.com/user-attachments/assets/60aab92b-76a3-4a78-871d-e3a83e1d5a10" />

## Table of contents

- [Project Background](#project-background)
- [Project Scope](#project-scope)
- [Current State](#current-state)
- [Feature Gallery](#feature-gallery)
- [Shortcuts and Syntax](#shortcuts-and-syntax)
- [Installation and Updating](#installation-and-updating)
- [Planned Features](#planned-features)
- [Privacy](#privacy)
- [Re: Project Background and Quality](#re-project-background-and-quality)

## Project Background

I started working on this project for 4 reasons:
- I was tired of Windows Explorers lack of features and slow speed
- I was also not a fan of the proprietary nature of its superior Windows alternatives (FilePilot, Directory Opus etc.)
- I wanted to experience the capabilities of current chatbots in coding
- I wanted to familiarize myself with the Rust programming language

My take on each of these objectives (especially the use of AI) will be found at the [bottom](#re-project-background-and-quality) of this README.

## Project Scope

FileFly was born out of a deep disdain for Windows File Explorer's low directory loading speeds, high resource usage and lack of features which should be common place in modern file managers. In addition, I felt the need to start this project as current, high performance alternatives such as [File Pilot](https://filepilot.tech/), although deeply impressive, are closed-source and paid.

Thus, the goal for FileFly is to implement features I personally desire in Windows File Explorers while maintaining cross-compatibility with Linux systems, all the while being much less RAM intensive.

## Current State

FileFly is a WiP. However, I do daily drive it for a number of features:

### Search: 
In my Testing, FileFly searches through Windows directories about 1000x faster than Windows File Explorer. It also supports global (system wide) search

### Resources:
In my Testing, FileFly uses between 3-5x less RAM than the Windows File Explorer in regular use. Thus, it usually stays below 100MB RAM usage, and often below 60MB too. It is also possible to further reduce the RAM usage in the indexer cache section of the settings.

### Panning: 
FileFly supports intelligent panning, with up to 4 directories being open at once. "Intelligent" here refers to the panes automatically being split along the shorter axis.

### Folder Sizes:
To obtain folder sizes in Windows File Explorer, one has to individually observe the properties of each folder. In addition, Windows takes a very long time to calculate them. On modern systems, this is unacceptable. Therefore, FileFly indexes folder sizes automatically and saves them in a compact binary format. Folder sizes are shown in the same Row as file sizes.
### WSL (Windows Subsystem for Linux):
WSL support is important to me for development. FileFly not only correctly resolves WSL paths, but also supports network drives in general. As usual, it also opens WSL directories much faster than the Windows File Explorer.
### Git integration: 
FileFly shows your current local branch and allows you to checkout different local branches, should you not have unstaged changes for the current branch (This feature has to be turned on in the Settings).
### Quick access:
FileFly offers a customizable quick access list. You can pin folders AND files to it. It also supports the selection of custom folder icons.
### Right-click context menu:
FileFly supports many actions through a custom right click context menu. Those include file operations, as well as support for PeaZip and the use of customization options such as custom folder icons.
### Full UTF-8 support:
FileFly supports all UTF-8 names for files and folders.
### Trash bin:
FileFly allows access to the native OS trash bin via a sidebar button.
### Session persistence
FileFly opens the same tabs/panes as when you closed it.
### OS paste bin
On Windows, FileFly uses the Windows paste bin. This means you can copy files in another app and paste them into FileFly. You can also drag and drop files into and out of a FileFly window.
### And of course:
All common file operations, such as copy, paste, cut (move), delete, rename, drag to copy and multi-file operations. To prevent file loss and corruption, operations like cut (move) are implemented safely and if FileFly crashes, leftover temp files will be deleted.

### In addition, FileFly aims to offer many customization options:
- Custom window frame: FileFly draws a custom window frame with different window control buttons for GNOME, KDE and Windows. You can toggle this feature.
- Transparency effects: On Windows, FileFly supports the Mica and Acrylic effects. You can turn these off if you prefer.
- Accent colors: FileFly supports your system accent color on Windows, as well as custom accent colors on Windows and Linux.
- Compact mode: FileFly offers a compact GUI option in the Settings.
- A collapsible sidebar.
- An optional drag space in the tab area.
- Support for optional paning blur effects.
- Font size, accessibility scaling and date/time format options.

## Feature Gallery
### Panning
<img width="1804" height="1328" alt="image" src="https://github.com/user-attachments/assets/f2311272-e328-451c-8ce6-75c289b57583" />
<br><br>
<img width="2880" height="1800" alt="image" src="https://github.com/user-attachments/assets/51c866fd-53b3-415d-b361-3467fa4f0de6" />

### Transparency

- Mica:
<img width="1804" height="1328" alt="image" src="https://github.com/user-attachments/assets/edb4708d-21e8-4053-8758-9a514ca9bda8" />
<br><br>

- Acrylic:
<img width="1804" height="1328" alt="image" src="https://github.com/user-attachments/assets/dc9c2222-ad92-4432-b9ba-1c361f454afa" />
<br><br>

- None:
<img width="1804" height="1328" alt="image" src="https://github.com/user-attachments/assets/ba8b7656-2fbf-450d-9117-b8e4d48a4f4a" />

### Light and dark modes
- Dark:
<img width="1804" height="1328" alt="image" src="https://github.com/user-attachments/assets/220eae20-4da8-44d7-a7da-4c3dcdae5081" />
<br><br>

- Light:
<img width="1804" height="1328" alt="image" src="https://github.com/user-attachments/assets/404cb686-d65d-4b4d-aa2a-18a4d3d4b04a" />

### Compact mode
<img width="1804" height="1328" alt="image" src="https://github.com/user-attachments/assets/bb4f0690-d82b-49be-abe9-95014e6cd829" />

### Git integration
<img width="1178" height="364" alt="image" src="https://github.com/user-attachments/assets/1de97a07-0024-4ae1-a633-856a59a7262f" />

### Right-click Context menu
<img width="500" height="1314" alt="image" src="https://github.com/user-attachments/assets/3f5f9432-6790-4424-af34-7e15e738f0db" />

## Shortcuts and Syntax

FileFly offers many shortcuts:
- Ctrl+T: New tab
- Ctrl+W: Close tab or pane
- Ctrl+C: Copy
- Ctrl+V: Paste
- Ctrl+X: Cut (behaves as copy in read only folders)
- Ctrl+F: Directory search
- Ctrl+G: System wide search
- Ctrl+S: Split the current active view into two panes
- Ctrl+Shift+N: New, empty and extensionless file
- Ctrl+Shift+F: New, empty folder
- Del: Delete selection (move to trash)
- F2: Rename selected file
- F5: Refresh view
- Alt+up-arrow: Go to parent directory
- Alt+back-arrow: Go back to the previous directory
- Alt+forward-arrow: Undo said back-arrow action

FileFly offers a specific Syntax as follows for its search:

*.ext

To find files with the given extension .ext.

## Installation and Updating
### Notes
FileFly is a free project. As such, I am not capable of paying to sign its executables. [Microsoft SmartScreen](https://en.wikipedia.org/wiki/Microsoft_SmartScreen) might show you a warning when executing FileFly or its installer. As FileFly is a public, fully open source project, it is up to you to trust it.
### Windows
#### Installation:
On Windows, FileFly is distributed as both a portable (FileFly.exe) as well as an installer. To install it, simply [download](https://github.com/SomewhatCls/FileFly/releases/latest/download/FileFlyInstaller.exe) and execute the latest installer and follow its steps. To uninstall, use the uninstaller in the install location, or the Windows settings.

#### Updating:
To update FileFly, simply download the newest installer. This can be done from here, or by pressing the "check" button in the update section of FileFly's settings. If an update was found, FileFly will then offer you a "Download" button, which will download the newest installer release from this github page. You will then have to execute said installer.

### Linux
#### Installation:
On Linux, FileFly is distributed as an AppImage. That means it will come with the necessary .desktop file, executable and App icon. However, you will have to unpack the AppImage and put these files in the appropriate locations if you wish to use them. Otherwise, You can also directly execute the AppImage by making it executable with the following command:

```bash
chmod a+x FileFly.AppImage
```
If you then wish to unpack it to access the files mentioned above, execute the following command:

```bash
./FileFly.AppImage --appimage-extract
```
The mentioned files will then be in a folder called "squashfs-root". From there, follow typical instructions for the use of .desktop entries.

#### Updating:
To update FileFly, simply download the newest AppImage. This can be done from here, or by pressing the "check" button in the update section of FileFly's settings. If an update was found, FileFly will then offer you a "Download" button, which will download the newest AppImage release from this github page. If updating a proper installation using a .desktop file, replace all extracted files. It is generally not necessary to replace the .desktop file, as it simply needs to point to your executable binary's location. However, it doesn't harm the app to do so.

## Planned Features

### Eject-able media formatting:
Currently, FileFly supports eject-able media for reading and writing, as well as updating its status live. However, I would like to allow users to open a formatting menu through the right click context menu.

### A more useful properties Window
Currently, the properties Window only offers information which you can observe in the file table, as well as the files creation date. I want it to offer more functionality regarding features such as toggling read-only, visibility and more.

### Support for other languages
Currently, FileFly only supports English. It does however support all UTF-8 characters in file and folder names.

### Default setting
Currently, file explorer does allow you to set it as your default File Explorer on Windows. However, this does not apply to file picker dialogues, as the file picker is spawned as a child process of an already open FileFly window instead of its own instance.

### Updates
As of now, FileFly supports fetching updates from its github source. Because FileFly is a free student project, it is not signed. Therefore, I am currently opening the browser to download the installer from its source on Windows, or the AppImage on Linux. I would like to improve this process to support proper installation without user input in the future.

### More customization
I wish to add more options for customization, regarding things such as fonts and backgrounds.

## Privacy
FileFly does NOT collect telemetry. FileFly does NOT share user data.

FileFly does save the following information on your device ONLY:
- Folder sizes (To display them in the file table)
- FileFly user settings
- Information about its session, to open the same tabs and to open the window at the same size as before.

In addition, FileFly will contact the Internet ONLY if you decide to check for updates. It is otherwise completely offline.

## Re: Project Background and Quality
A short preamble; At its current state, I believe this project has already proven both much faster and more efficient than Windows File Explorer, achieving results similar enough to those of proprietary projects such as File Pilot while being open source. At the same time, I find it file operation safe and stable enough to use it in my own daily use. However, this code was not written by an expert or a professional, but by a student.

I have long held that chatbots supplied me with insufficient code in languages I am well versed in. Although I am thoroughly impressed by Claude's ability to write absurdly more complex and functional code than previous chatbots I have tried in other years, I did not pay for access to it (which also means it does not have access to my code base). As such, this code has been increasingly written by me and my likely lacking Rust abilities. Nonetheless, the 10 or so minutes a day I get to use Claude for free are still highly useful in understanding the various crate APIs I implement. As such, while I am impressed by Claude's capabilities and while this project has achieved a much more performant and efficient result than the Windows File Explorer, current feature progress is gonna be determined by my Rust abilities.
