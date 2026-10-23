# Compilation facultative du moteur portable, sans installation systeme.
[CmdletBinding()]
param(
    [string]$WorkDirectory = (Join-Path $PSScriptRoot 'target\ocr-build'),
    [string]$Destination = (Join-Path $PSScriptRoot 'target\release\ocr')
)
$ErrorActionPreference = 'Stop'
$cmake = (Get-Command cmake -ErrorAction SilentlyContinue).Source
if (-not $cmake) {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path -LiteralPath $vswhere) {
        $cmake = & $vswhere -latest -products '*' -find 'Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe' | Select-Object -First 1
    }
}
if (-not $cmake -or -not (Get-Command tar -ErrorAction SilentlyContinue)) {
    throw 'CMake ou tar absent : voir le mini-tutoriel de compilation dans README.md.'
}
$WorkDirectory = [IO.Path]::GetFullPath($WorkDirectory)
$Destination = [IO.Path]::GetFullPath($Destination)
$sourceRoot = Join-Path $WorkDirectory 'sources'
$buildRoot = Join-Path $WorkDirectory 'build'
$prefix = Join-Path $WorkDirectory 'prefix'
$downloads = Join-Path $WorkDirectory 'downloads'
New-Item -ItemType Directory -Force $sourceRoot,$buildRoot,$prefix,$downloads | Out-Null
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

function Get-Verified([string]$Url, [string]$Path, [string]$Hash) {
    if (-not (Test-Path -LiteralPath $Path)) {
        Write-Host "Telechargement : $Url"
        Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $Path
    }
    if ((Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash -ne $Hash) {
        throw "SHA-256 incorrect : $Path. Supprime ce fichier avant de reessayer."
    }
}
$sources = @(
    @('zlib','zlib-1.3.2','https://github.com/madler/zlib/archive/refs/tags/v1.3.2.tar.gz','B99A0B86C0BA9360EC7E78C4F1E43B1CBDF1E6936C8FA0F6835C0CD694A495A1'),
    @('libpng','libpng-1.6.58','https://github.com/pnggroup/libpng/archive/refs/tags/v1.6.58.tar.gz','A9D4DF463D36A6E5F9C29BD6F4967312D17E996C1854F3511F833924EB1993CF'),
    @('libjpeg','libjpeg-turbo-3.1.3','https://github.com/libjpeg-turbo/libjpeg-turbo/releases/download/3.1.3/libjpeg-turbo-3.1.3.tar.gz','075920B826834AC4DDF97661CC73491047855859AFFD671D52079C6867C1C6C0'),
    @('libtiff','tiff-4.7.2','https://download.osgeo.org/libtiff/tiff-4.7.2.tar.gz','672BD7D10AEE4606171AFB864F3570B83340F6A33E2C186DC0512F7145FFDF6A'),
    @('leptonica','leptonica-1.87.0','https://github.com/DanBloomberg/leptonica/archive/refs/tags/1.87.0.tar.gz','FA2B40C5CAEA96D1BB93A97486262AED8731B69CE25A84A6BF5D25323E33F631'),
    @('tesseract','tesseract-5.5.3','https://github.com/tesseract-ocr/tesseract/archive/refs/tags/5.5.3.tar.gz','9218E62793116D42A9F6D14CD9348518B27F382096EEA3D0F2D1A24616BB5884')
)
foreach ($item in $sources) {
    $archive = Join-Path $downloads ($item[0] + '.tar.gz')
    Get-Verified $item[2] $archive $item[3]
    if (-not (Test-Path -LiteralPath (Join-Path $sourceRoot $item[1]))) {
        & tar -xzf $archive -C $sourceRoot
        if ($LASTEXITCODE -ne 0) { throw "Extraction echouee : $archive" }
    }
}

function Build-Library([string]$Name, [string]$Source, [string[]]$Extra) {
    $build = Join-Path $buildRoot $Name
    $log = Join-Path $buildRoot "$Name.log"
    Write-Host "Compilation : $Name"
    $arguments = @('-S',(Join-Path $sourceRoot $Source),'-B',$build,'-A','x64',
        "-DCMAKE_INSTALL_PREFIX=$prefix","-DCMAKE_PREFIX_PATH=$prefix",'-DCMAKE_BUILD_TYPE=Release',
        '-DCMAKE_POLICY_DEFAULT_CMP0091=NEW','-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded','-DBUILD_SHARED_LIBS=OFF',
        "-DZLIB_LIBRARY=$prefix/lib/zs.lib","-DZLIB_INCLUDE_DIR=$prefix/include") + $Extra
    & $cmake @arguments *> $log
    if ($LASTEXITCODE -ne 0) { Get-Content $log -Tail 30; throw "Configuration echouee : $Name" }
    & $cmake --build $build --config Release --parallel 1 -- /nodeReuse:false *>> $log
    if ($LASTEXITCODE -ne 0) { Get-Content $log -Tail 30; throw "Compilation echouee : $Name" }
    & $cmake --install $build --config Release *>> $log
    if ($LASTEXITCODE -ne 0) { Get-Content $log -Tail 30; throw "Copie locale echouee : $Name" }
}
Build-Library 'zlib' 'zlib-1.3.2' @('-DZLIB_BUILD_SHARED=OFF','-DZLIB_BUILD_STATIC=ON','-DZLIB_BUILD_TESTING=OFF')
Build-Library 'libpng' 'libpng-1.6.58' @('-DPNG_SHARED=OFF','-DPNG_STATIC=ON','-DPNG_TESTS=OFF','-DPNG_TOOLS=OFF')
Build-Library 'libjpeg' 'libjpeg-turbo-3.1.3' @('-DENABLE_SHARED=OFF','-DENABLE_STATIC=ON',
    '-DWITH_TURBOJPEG=OFF','-DWITH_TOOLS=OFF','-DWITH_TESTS=OFF','-DWITH_SIMD=OFF','-DWITH_CRT_DLL=OFF')
Build-Library 'libtiff' 'tiff-4.7.2' @('-Dtiff-static=ON','-Dtiff-tools=OFF','-Dtiff-tests=OFF',
    '-Dtiff-contrib=OFF','-Dtiff-docs=OFF','-Dtiff-cxx=OFF','-Dtiff-opengl=OFF',
    '-Djpeg=OFF','-Dold-jpeg=OFF','-Djbig=OFF','-Dlerc=OFF','-Dlzma=OFF','-Dzstd=OFF','-Dwebp=OFF',
    '-Dlibdeflate=OFF','-Dzlib=OFF','-Dpixarlog=OFF')
Build-Library 'leptonica' 'leptonica-1.87.0' @('-DSW_BUILD=OFF','-DBUILD_PROG=OFF','-DENABLE_PNG=ON','-DENABLE_ZLIB=ON',
    '-DENABLE_GIF=OFF','-DENABLE_JPEG=ON','-DENABLE_TIFF=ON','-DENABLE_WEBP=OFF','-DENABLE_OPENJPEG=OFF',
    "-DJPEG_LIBRARY=$prefix/lib/jpeg-static.lib","-DJPEG_INCLUDE_DIR=$prefix/include")
Build-Library 'tesseract' 'tesseract-5.5.3' @('-DBUILD_TRAINING_TOOLS=OFF','-DBUILD_TESTS=OFF','-DDISABLE_TIFF=ON',
    '-DDISABLE_ARCHIVE=ON','-DDISABLE_CURL=ON','-DGRAPHICS_DISABLED=ON','-DENABLE_NATIVE=OFF','-DOPENMP_BUILD=OFF','-DINSTALL_CONFIGS=ON')

$data = Join-Path $Destination 'tessdata'
$configs = Join-Path $data 'configs'
$licenses = Join-Path $Destination 'licences'
New-Item -ItemType Directory -Force $Destination,$data,$configs,$licenses | Out-Null
Copy-Item -LiteralPath (Join-Path $prefix 'bin\tesseract.exe') -Destination $Destination
foreach ($config in @('txt','tsv')) {
    Copy-Item -LiteralPath (Join-Path $sourceRoot "tesseract-5.5.3\tessdata\configs\$config") -Destination $configs
}
$modelCommit = '87416418657359cb625c412a48b6e1d6d41c29bd'
$modelUrl = "https://raw.githubusercontent.com/tesseract-ocr/tessdata_fast/$modelCommit"
$models = @(
    @('fra','CED037562E8C80C13122DECE28DD477D399AF80911A28791A66A63AC1E3445CA'),
    @('eng','7D4322BD2A7749724879683FC3912CB542F19906C83BCC1A52132556427170B2')
)
foreach ($model in $models) {
    $file = $model[0] + '.traineddata'
    $cached = Join-Path $downloads $file
    Get-Verified "$modelUrl/$file" $cached $model[1]
    Copy-Item -LiteralPath $cached -Destination $data
}
Copy-Item -LiteralPath (Join-Path $sourceRoot 'tesseract-5.5.3\LICENSE') -Destination (Join-Path $licenses 'Tesseract-et-modeles-Apache-2.0.txt')
Copy-Item -LiteralPath (Join-Path $sourceRoot 'leptonica-1.87.0\leptonica-license.txt') -Destination (Join-Path $licenses 'Leptonica-BSD.txt')
Copy-Item -LiteralPath (Join-Path $sourceRoot 'libpng-1.6.58\LICENSE') -Destination (Join-Path $licenses 'libpng.txt')
Copy-Item -LiteralPath (Join-Path $sourceRoot 'tiff-4.7.2\LICENSE.md') -Destination (Join-Path $licenses 'libtiff.txt')
Copy-Item -LiteralPath (Join-Path $sourceRoot 'libjpeg-turbo-3.1.3\LICENSE.md') -Destination (Join-Path $licenses 'libjpeg-turbo.txt')
Copy-Item -LiteralPath (Join-Path $sourceRoot 'libjpeg-turbo-3.1.3\README.ijg') -Destination (Join-Path $licenses 'libjpeg-IJG.txt')
Copy-Item -LiteralPath (Join-Path $sourceRoot 'zlib-1.3.2\LICENSE') -Destination (Join-Path $licenses 'zlib.txt')
$notice = @(
    'Moteur OCR fourni avec HDV Captures. Sources officielles non modifiees.',
    'Compilation Windows x64 statique : PNG, texte et TSV ; sans telechargement pendant utilisation.',
    'Tesseract : Copyright (C) 2016 Google, Inc. et contributeurs ; licence Apache 2.0.',
    'This software is based in part on the work of the Independent JPEG Group.',
    'Modeles tessdata_fast : licence Apache 2.0. Leptonica : BSD. libpng, libtiff, libjpeg-turbo et zlib : licences jointes.',
    '', 'Sources et SHA-256 des archives :'
)
foreach ($item in $sources) { $notice += "$($item[1])`r`n$($item[2])`r`nSHA-256 : $($item[3])" }
$notice += "`r`nModeles : https://github.com/tesseract-ocr/tessdata_fast/tree/$modelCommit"
foreach ($model in $models) { $notice += "$($model[0]).traineddata SHA-256 : $($model[1])" }
$notice += "`r`nReconstruction : Compiler-OCR.ps1 dans le ZIP de sources de HDV Captures."
Set-Content -LiteralPath (Join-Path $Destination 'NOTICE.txt') -Value $notice -Encoding UTF8
& (Join-Path $Destination 'tesseract.exe') --tessdata-dir $data --list-langs
if ($LASTEXITCODE -ne 0) { throw 'Verification finale du moteur OCR echouee.' }
Write-Host "Moteur portable : $Destination" -ForegroundColor Green
Get-FileHash -LiteralPath (Join-Path $Destination 'tesseract.exe') -Algorithm SHA256
