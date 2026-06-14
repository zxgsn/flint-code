[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$body = @{
    model = "agnes-image-2.1-flash"
    messages = @(
        @{role = "user"; content = "test"}
    )
    max_tokens = 100
} | ConvertTo-Json -Compress

$headers = @{
    "Content-Type" = "application/json"
    "Authorization" = "Bearer sk-IoXSms4h5mSDDWV4m3yX057g1SP5tvAkrxcNKntmKZA06pQ1"
}

try {
    $response = Invoke-RestMethod -Uri "https://apihub.agnes-ai.com/v1/chat/completions" -Method Post -Body ([System.Text.Encoding]::UTF8.GetBytes($body)) -Headers $headers
    $response | ConvertTo-Json -Depth 10
} catch {
    Write-Error $_.Exception.Message
    if ($_.Exception.Response) {
        $reader = New-Object IO.StreamReader($_.Exception.Response.GetResponseStream())
        $reader.BaseStream.Position = 0
        $reader.DiscardRemainingContents()
        Write-Error $reader.ReadToEnd()
    }
}
